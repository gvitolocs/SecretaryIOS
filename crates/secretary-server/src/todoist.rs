//! Todoist: the official API v1 client (`/api/v1/sync`, completed tasks,
//! user, OAuth) and the Links connection (`/v1/links/todoist/*`).
//!
//! The owner connects in Links either with Todoist's OAuth (when a Todoist
//! app is configured: `SECRETARY_TODOIST_CLIENT_ID_FILE` + `_SECRET_FILE`)
//! or by pasting the personal API token (Todoist → Settings → Integrations
//! → Developer). Either token is sealed with `links.key`
//! ([`crate::secrets`], purposes `todoist-token` / `todoist-refresh`) in
//! `links.json` and never returned to clients.
//!
//! - `POST /v1/links/todoist/token` `{token}`: checked against
//!   `GET /api/v1/user` first; 422 `todoist_token_rejected` when Todoist
//!   refuses it.
//! - `POST /v1/links/todoist/connect` `{client?}` → `{authorizeUrl}`;
//!   Todoist redirects to `GET /v1/links/todoist/callback` (public; the
//!   single-use `state` binds it), which stores the token and returns to
//!   `/links?todoist=connected` (or `secretary://links?…` for iOS).
//! - `DELETE /v1/links/todoist`: forget the token (OAuth tokens are revoked
//!   at Todoist) and drop the synced data; local-only tasks stay.
//! - A token Todoist stops accepting flips the card to `reconnect`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{ApiError, JsonBody};
use crate::links::{now_iso, ConnStatus};
use crate::AppState;

pub const API_BASE: &str = "https://api.todoist.com";
pub const AUTH_BASE: &str = "https://app.todoist.com";
pub const DEFAULT_REDIRECT: &str = "https://getsecretary.tech/v1/links/todoist/callback";
pub const SCOPE: &str = "data:read_write,data:delete";
/// What `/api/v1/sync` is asked for.
pub const RESOURCES: &str = r#"["items","projects","sections","labels","filters","user","stats"]"#;
const PENDING_TTL: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct TodoistOAuth {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

#[derive(Clone)]
pub struct TodoistConfig {
    pub api_base: String,
    pub auth_base: String,
    pub oauth: Option<TodoistOAuth>,
    /// Background sync interval; zero turns the loop off.
    pub sync_every: Duration,
}

impl Default for TodoistConfig {
    fn default() -> Self {
        Self {
            api_base: API_BASE.into(),
            auth_base: AUTH_BASE.into(),
            oauth: None,
            sync_every: Duration::from_secs(90),
        }
    }
}

impl std::fmt::Debug for TodoistConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TodoistConfig")
            .field("api_base", &self.api_base)
            .field("oauth", &self.oauth.as_ref().map(|_| "<set>"))
            .field("sync_every", &self.sync_every)
            .finish()
    }
}

/// The stored connection (in `links.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoistConn {
    /// `token` (personal API token) or `oauth`.
    pub method: String,
    /// Sealed (`todoist-token`).
    pub token: String,
    /// Sealed (`todoist-refresh`), OAuth apps with refresh tokens only.
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub account: Option<String>,
    pub connected_at: String,
    #[serde(default)]
    pub status: ConnStatus,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_sync_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TdError {
    NotConnected,
    /// 401/403: the token no longer works.
    Unauthorized,
    /// 429, with the seconds to wait.
    RateLimited(u64),
    Http(u16, String),
    Network(String),
}

impl std::fmt::Display for TdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => write!(f, "Todoist is not connected"),
            Self::Unauthorized => write!(f, "Todoist refused the token"),
            Self::RateLimited(s) => write!(f, "Todoist rate limit; retry in {s} s"),
            Self::Http(c, m) => write!(f, "Todoist answered {c}: {m}"),
            Self::Network(m) => write!(f, "Todoist unreachable: {m}"),
        }
    }
}

pub struct Client {
    pub cfg: TodoistConfig,
    http: reqwest::Client,
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

impl Client {
    pub fn new(cfg: TodoistConfig) -> anyhow::Result<Self> {
        Ok(Self {
            cfg,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!(
                    "secretary-server/",
                    env!("CARGO_PKG_VERSION"),
                    " (tasks)"
                ))
                .build()?,
        })
    }

    async fn call(
        &self,
        token: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        form: Option<&[(&str, String)]>,
    ) -> Result<Value, TdError> {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.cfg.api_base))
            .bearer_auth(token)
            .query(query);
        if let Some(f) = form {
            req = req.form(f);
        }
        let resp = req.send().await.map_err(|e| {
            TdError::Network(if e.is_timeout() {
                "timeout".into()
            } else {
                short(&e.without_url().to_string())
            })
        })?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(TdError::Unauthorized);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let extra = body
                .get("error_extra")
                .and_then(|e| e.get("retry_after"))
                .and_then(Value::as_u64);
            return Err(TdError::RateLimited(
                header.or(extra).unwrap_or(60).clamp(1, 3600),
            ));
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(TdError::Http(status.as_u16(), short(&text)));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| TdError::Http(status.as_u16(), short(&format!("bad JSON: {e}"))))
    }

    /// `POST /api/v1/sync`: a pull (`commands` empty) or a push.
    pub async fn sync(
        &self,
        token: &str,
        sync_token: Option<&str>,
        commands: &[Value],
    ) -> Result<Value, TdError> {
        let mut form = vec![
            ("sync_token", sync_token.unwrap_or("*").to_owned()),
            ("resource_types", RESOURCES.to_owned()),
        ];
        if !commands.is_empty() {
            form.push(("commands", Value::Array(commands.to_vec()).to_string()));
        }
        self.call(token, Method::POST, "/api/v1/sync", &[], Some(&form))
            .await
    }

    /// Completed tasks between two instants (RFC 3339).
    pub async fn completed(
        &self,
        token: &str,
        since: &str,
        until: &str,
    ) -> Result<Vec<Value>, TdError> {
        let v = self
            .call(
                token,
                Method::GET,
                "/api/v1/tasks/completed/by_completion_date",
                &[
                    ("since", since.to_owned()),
                    ("until", until.to_owned()),
                    ("limit", "200".to_owned()),
                ],
                None,
            )
            .await?;
        Ok(v.get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// The account behind a token (`GET /api/v1/user`).
    pub async fn user(&self, token: &str) -> Result<Value, TdError> {
        self.call(token, Method::GET, "/api/v1/user", &[], None)
            .await
    }

    pub fn authorize_url(&self, state: &str) -> Option<String> {
        let o = self.cfg.oauth.as_ref()?;
        let mut u = reqwest::Url::parse(&format!("{}/oauth/authorize", self.cfg.auth_base)).ok()?;
        u.query_pairs_mut()
            .append_pair("client_id", &o.client_id)
            .append_pair("scope", SCOPE)
            .append_pair("state", state)
            .append_pair("redirect_uri", &o.redirect_uri)
            .append_pair("response_type", "code");
        Some(u.to_string())
    }

    /// Code or refresh-token grant at `/oauth/access_token`.
    async fn token_grant(&self, form: &[(&str, String)]) -> Result<TokenAnswer, TdError> {
        let resp = self
            .http
            .post(format!("{}/oauth/access_token", self.cfg.api_base))
            .form(form)
            .send()
            .await
            .map_err(|e| TdError::Network(short(&e.without_url().to_string())))?;
        let status = resp.status();
        if status.is_client_error() {
            return Err(TdError::Unauthorized);
        }
        if !status.is_success() {
            return Err(TdError::Http(status.as_u16(), String::new()));
        }
        resp.json::<TokenAnswer>()
            .await
            .map_err(|e| TdError::Http(status.as_u16(), short(&e.to_string())))
    }

    pub async fn exchange(&self, code: &str) -> Result<TokenAnswer, TdError> {
        let o = self.cfg.oauth.as_ref().ok_or(TdError::NotConnected)?;
        self.token_grant(&[
            ("client_id", o.client_id.clone()),
            ("client_secret", o.client_secret.clone()),
            ("code", code.to_owned()),
            ("redirect_uri", o.redirect_uri.clone()),
        ])
        .await
    }

    pub async fn refresh(&self, refresh: &str) -> Result<TokenAnswer, TdError> {
        let o = self.cfg.oauth.as_ref().ok_or(TdError::NotConnected)?;
        self.token_grant(&[
            ("client_id", o.client_id.clone()),
            ("client_secret", o.client_secret.clone()),
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh.to_owned()),
        ])
        .await
    }

    /// RFC 7009 revoke (OAuth tokens only).
    pub async fn revoke(&self, token: &str) -> bool {
        let Some(o) = self.cfg.oauth.as_ref() else {
            return false;
        };
        self.http
            .post(format!("{}/api/v1/revoke", self.cfg.api_base))
            .basic_auth(&o.client_id, Some(&o.client_secret))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[derive(Debug, Deserialize)]
pub struct TokenAnswer {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

struct Pending {
    state_hash: [u8; 32],
    client: String,
    created: Instant,
}

/// Pending OAuth states (memory only, 10 minutes).
#[derive(Default)]
pub struct PendingStates(Mutex<Vec<Pending>>);

fn sha(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

// -- the token ---------------------------------------------------------------------

/// A usable access token: refreshed when an OAuth token is about to
/// expire. `Unauthorized` marks the connection `reconnect`.
pub async fn access_token(st: &AppState) -> Result<String, TdError> {
    let hub = &st.links;
    let conn = {
        let d = hub.data.lock().await;
        d.links.todoist.clone().ok_or(TdError::NotConnected)?
    };
    if conn.status == ConnStatus::Reconnect {
        return Err(TdError::Unauthorized);
    }
    let token = hub
        .sealer
        .open("todoist-token", &conn.token)
        .map_err(|_| TdError::Unauthorized)?;
    let now = chrono::Utc::now().timestamp_millis();
    let expiring = conn.expires_at_ms.is_some_and(|e| e - 120_000 < now);
    let Some(refresh_sealed) = conn.refresh_token.as_ref().filter(|_| expiring) else {
        return Ok(token);
    };
    let refresh = hub
        .sealer
        .open("todoist-refresh", refresh_sealed)
        .map_err(|_| TdError::Unauthorized)?;
    let tok = match st.tasks.client.refresh(&refresh).await {
        Ok(t) => t,
        Err(TdError::Unauthorized) => {
            mark_reconnect(st, "Todoist access expired; reconnect in Links").await;
            return Err(TdError::Unauthorized);
        }
        Err(e) => return Err(e),
    };
    let mut d = hub.data.lock().await;
    if let Some(c) = d.links.todoist.as_mut() {
        if let Ok(s) = hub.sealer.seal("todoist-token", &tok.access_token) {
            c.token = s;
        }
        // refresh tokens rotate on every use
        if let Some(r) = &tok.refresh_token {
            if let Ok(s) = hub.sealer.seal("todoist-refresh", r) {
                c.refresh_token = Some(s);
            }
        }
        c.expires_at_ms = tok.expires_in.map(|s| now + s * 1000);
    }
    let _ = hub.save_links(&d);
    Ok(tok.access_token)
}

pub async fn mark_reconnect(st: &AppState, why: &str) {
    let hub = &st.links;
    {
        let mut d = hub.data.lock().await;
        if let Some(c) = d.links.todoist.as_mut() {
            c.status = ConnStatus::Reconnect;
            c.last_error = Some(why.to_owned());
        }
        let _ = hub.save_links(&d);
    }
    let why = why.to_owned();
    let _ = crate::engines::with_engine(&st.tasks.engine, move |e| {
        e.set_sync_status(secretary_tasks::SyncStatus::Reconnect, Some(why), None)
    })
    .await;
}

pub async fn note_sync(st: &AppState, error: Option<String>) {
    let mut d = st.links.data.lock().await;
    if let Some(c) = d.links.todoist.as_mut() {
        if error.is_none() {
            c.last_sync_at = Some(now_iso());
            c.status = ConnStatus::Ok;
        }
        c.last_error = error;
    }
    let _ = st.links.save_links(&d);
}

// -- Links card --------------------------------------------------------------------

/// The Todoist card for `GET /v1/links` (group `tasks`).
pub fn card(conn: Option<&TodoistConn>, oauth: bool, open: u32, pending: u32) -> Value {
    let status = match conn {
        None => "disconnected",
        Some(c) if c.status == ConnStatus::Reconnect => "reconnect",
        Some(c) if c.last_error.is_some() => "error",
        Some(_) => "connected",
    };
    let mut v = json!({
        "id": "todoist", "kind": "todoist", "name": "Todoist", "status": status,
        "oauth": oauth,
        "scopes": ["data:read_write"],
    });
    v["actions"] = json!(match (status, oauth) {
        ("connected" | "error", _) => vec!["sync", "disconnect"],
        ("reconnect", true) => vec!["reconnect", "token", "disconnect"],
        ("reconnect", false) => vec!["token", "disconnect"],
        (_, true) => vec!["connect", "token"],
        _ => vec!["token"],
    });
    match conn {
        Some(c) => {
            v["account"] = json!(c.account);
            v["lastSyncAt"] = json!(c.last_sync_at);
            v["error"] = json!(c.last_error);
            v["method"] = json!(c.method);
            v["count"] = json!(open);
            v["detail"] = json!(if pending > 0 {
                format!("{open} open tasks · {pending} edits waiting")
            } else {
                format!("{open} open tasks")
            });
        }
        None => {
            v["detail"] = json!(if oauth {
                "Connect with Todoist, or paste your API token (Todoist → Settings → Integrations → Developer)"
            } else {
                "Paste your API token (Todoist → Settings → Integrations → Developer)"
            });
        }
    }
    v
}

// -- routes ------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenBody {
    token: String,
}

fn bad(code: &str, msg: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, msg)
}

/// Store a working token, reset the engine when the account changed and
/// start a sync.
async fn store(
    st: &AppState,
    method: &str,
    token: &str,
    refresh: Option<&str>,
    expires_in: Option<i64>,
    account: Option<String>,
) -> Result<(), ApiError> {
    let hub = &st.links;
    let seal = |purpose: &str, v: &str| {
        hub.sealer.seal(purpose, v).map_err(|e| {
            tracing::error!(error = %e, "sealing failed");
            ApiError::internal("could not encrypt the token")
        })
    };
    let conn = TodoistConn {
        method: method.into(),
        token: seal("todoist-token", token)?,
        refresh_token: refresh.map(|r| seal("todoist-refresh", r)).transpose()?,
        expires_at_ms: expires_in
            .filter(|s| *s < 315_360_000)
            .map(|s| chrono::Utc::now().timestamp_millis() + s * 1000),
        account: account.clone(),
        connected_at: now_iso(),
        status: ConnStatus::Ok,
        last_error: None,
        last_sync_at: None,
    };
    {
        let mut d = hub.data.lock().await;
        d.links.todoist = Some(conn);
        hub.save_links(&d)?;
    }
    crate::engines::with_engine(&st.tasks.engine, move |e| e.connect(account))
        .await?
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", e))?;
    tracing::info!(method, "todoist connected");
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = crate::tasks::sync_once(&st2).await;
    });
    Ok(())
}

fn account_of(user: &Value) -> Option<String> {
    user.get("email")
        .or_else(|| user.get("full_name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

async fn token_connect(
    State(st): State<AppState>,
    JsonBody(b): JsonBody<TokenBody>,
) -> Result<Json<Value>, ApiError> {
    let token = b.token.trim();
    if token.len() < 20
        || token.len() > 256
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad(
            "bad_request",
            "token: paste the API token from Todoist (Settings → Integrations → Developer)",
        ));
    }
    let user = match st.tasks.client.user(token).await {
        Ok(u) => u,
        Err(TdError::Unauthorized) => {
            return Err(bad(
                "todoist_token_rejected",
                "Todoist did not accept that token",
            ))
        }
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "todoist_unavailable",
                e.to_string(),
            ))
        }
    };
    let account = account_of(&user);
    store(&st, "token", token, None, None, account.clone()).await?;
    Ok(Json(json!({"connected": true, "account": account})))
}

#[derive(Deserialize, Default)]
struct ConnectBody {
    #[serde(default)]
    client: Option<String>,
}

async fn oauth_connect(
    State(st): State<AppState>,
    JsonBody(b): JsonBody<ConnectBody>,
) -> Result<Json<Value>, ApiError> {
    let client = match b.client.as_deref() {
        None | Some("web") => "web",
        Some("ios") => "ios",
        Some(_) => return Err(crate::links::bad("client: web or ios")),
    };
    let state = crate::links::random_secret()?;
    let url = st.tasks.client.authorize_url(&state).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "todoist_oauth_not_configured",
            "Todoist sign-in is not configured on the server; paste the API token instead",
        )
    })?;
    let mut p = st.tasks.pending.0.lock().unwrap_or_else(|e| e.into_inner());
    p.retain(|x| x.created.elapsed() < PENDING_TTL);
    if p.len() >= 5 {
        p.remove(0);
    }
    p.push(Pending {
        state_hash: sha(&state),
        client: client.into(),
        created: Instant::now(),
    });
    Ok(Json(json!({"authorizeUrl": url})))
}

fn back(client: &str, outcome: &str, reason: Option<&str>) -> Response {
    let base = if client == "ios" {
        "secretary://links"
    } else {
        "/links"
    };
    let mut url = format!("{base}?todoist={outcome}");
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

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oauth_callback(State(st): State<AppState>, Query(q): Query<CallbackQuery>) -> Response {
    let Some(state) = q.state.as_deref().filter(|s| s.len() <= 128) else {
        return back("web", "error", Some("missing_state"));
    };
    let pending = {
        let mut p = st.tasks.pending.0.lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|x| x.created.elapsed() < PENDING_TTL);
        let h = sha(state);
        let pos = p
            .iter()
            .position(|x| bool::from(subtle::ConstantTimeEq::ct_eq(&x.state_hash[..], &h[..])));
        pos.map(|i| p.remove(i))
    };
    let Some(pending) = pending else {
        return back("web", "error", Some("expired_or_unknown_state"));
    };
    if let Some(e) = q.error.as_deref() {
        return back(&pending.client, "error", Some(e));
    }
    let Some(code) = q.code.as_deref().filter(|c| c.len() <= 2048) else {
        return back(&pending.client, "error", Some("missing_code"));
    };
    let tok = match st.tasks.client.exchange(code).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "todoist code exchange failed");
            return back(&pending.client, "error", Some("exchange_failed"));
        }
    };
    let account = st
        .tasks
        .client
        .user(&tok.access_token)
        .await
        .ok()
        .as_ref()
        .and_then(account_of);
    match store(
        &st,
        "oauth",
        &tok.access_token,
        tok.refresh_token.as_deref(),
        tok.expires_in,
        account,
    )
    .await
    {
        Ok(()) => back(&pending.client, "connected", None),
        Err(_) => back(&pending.client, "error", Some("storage")),
    }
}

async fn disconnect(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let conn = {
        let mut d = st.links.data.lock().await;
        let c = d.links.todoist.take();
        st.links.save_links(&d)?;
        c
    };
    let mut revoked = false;
    if let Some(c) = conn.filter(|c| c.method == "oauth") {
        if let Ok(t) = st.links.sealer.open("todoist-token", &c.token) {
            revoked = st.tasks.client.revoke(&t).await;
        }
    }
    crate::engines::with_engine(&st.tasks.engine, |e| e.disconnect())
        .await?
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", e))?;
    tracing::info!("todoist disconnected");
    Ok(Json(
        json!({"disconnected": true, "revokedAtTodoist": revoked}),
    ))
}

/// Owner-only Links routes (device auth; behind `require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/links/todoist/token", post(token_connect))
        .route("/v1/links/todoist/connect", post(oauth_connect))
        .route("/v1/links/todoist/sync", post(crate::tasks::sync_now))
        .route("/v1/links/todoist", delete(disconnect))
        .route_layer(axum::middleware::from_fn(crate::links::guard))
}

/// The OAuth callback (no auth: the `state` binds it).
pub fn public_routes() -> Router<AppState> {
    Router::new().route("/v1/links/todoist/callback", get(oauth_callback))
}
