//! OAuth 2.1 authorization server for ChatGPT custom connectors (and any
//! other MCP client that does OAuth), per the MCP authorization spec
//! (2026-07-28, also 2025-11-25):
//!
//! - RFC 9728 protected resource metadata, RFC 8414 server metadata.
//! - Clients: Client ID Metadata Documents (CIMD; only under the
//!   `SECRETARY_MCP_OAUTH_CIMD` URL prefixes, ChatGPT's by default) and RFC
//!   7591 dynamic registration (`/oauth/register`) as the fallback.
//! - Authorization code + PKCE S256 only; `iss` in every authorization
//!   response (RFC 9207); RFC 8707 `resource` must be this server's `/mcp`.
//! - `redirect_uri`s: https only, and only the allowed ones (ChatGPT's two
//!   callbacks by default).
//! - The owner signs in with the Secretary email + password, checked by
//!   secretary-server (`POST /v1/session`), with login rate limits here on
//!   top of the server's own lockout.
//! - Access tokens (`mcpa_…`, 1 h) and rotating refresh tokens (`mcpr_…`,
//!   30 d) are stored as SHA-256 in `oauth.json` (0600). Reusing a rotated
//!   refresh token, or an authorization code twice, revokes the whole grant.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form, Json};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::backend::OwnerCheck;
use crate::config::Config;
use crate::login;
use crate::ratelimit::Limiter;
use crate::scope::{Caller, CallerKind, Scope, Scopes};
use crate::store::{random_id, random_secret, sha256_b64url, sha256_hex, unix_now, JsonFile};
use crate::AppState;

pub const ACCESS_PREFIX: &str = "mcpa_";
pub const REFRESH_PREFIX: &str = "mcpr_";
const FILE: &str = "oauth.json";
const LOCK: &str = "oauth.lock";
const CODE_TTL: Duration = Duration::from_secs(300);
const PENDING_TTL: Duration = Duration::from_secs(900);
const CIMD_CACHE: Duration = Duration::from_secs(3600);
const CIMD_MAX_BYTES: usize = 32 * 1024;
const MAX_DCR_CLIENTS: usize = 50;

// -- stored state -------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthData {
    #[serde(default)]
    pub clients: Vec<RegisteredClient>,
    #[serde(default)]
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub tokens: Vec<TokenRec>,
}

/// A dynamically registered client (RFC 7591).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredClient {
    pub client_id: String,
    #[serde(default)]
    pub secret_hash: Option<String>,
    /// `none`, `client_secret_post` or `client_secret_basic`.
    pub auth_method: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub caller: String,
    pub created_at: u64,
}

/// One owner consent: the tokens hang off it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub client_id: String,
    pub client_name: String,
    pub caller: String,
    pub scopes: Scopes,
    pub resource: String,
    pub created_at: u64,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Access,
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRec {
    pub hash: String,
    pub kind: TokenKind,
    pub grant_id: String,
    pub scopes: Scopes,
    pub expires_at: u64,
    /// Refresh tokens: already rotated (a second use is a replay).
    #[serde(default)]
    pub used: bool,
}

// -- in-memory state ------------------------------------------------------------

/// A client as the authorization endpoints see it.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub auth_method: String,
    pub secret_hash: Option<String>,
    pub caller: String,
}

/// A validated authorization request waiting for the owner's login.
#[derive(Debug, Clone)]
struct Pending {
    client: ClientInfo,
    redirect_uri: String,
    state: Option<String>,
    code_challenge: String,
    scopes: Scopes,
    resource: String,
    created: Instant,
}

#[derive(Debug, Clone)]
struct CodeRec {
    client_id: String,
    client_name: String,
    caller: String,
    redirect_uri: String,
    code_challenge: String,
    scopes: Scopes,
    resource: String,
    created: Instant,
    /// Set once redeemed: the grant to revoke on a replay.
    redeemed: Option<String>,
}

pub struct OAuth {
    file: JsonFile<OAuthData>,
    pending: Mutex<HashMap<String, Pending>>,
    codes: Mutex<HashMap<String, CodeRec>>,
    cimd: Mutex<HashMap<String, (Instant, ClientInfo)>>,
    http: reqwest::Client,
    login_ip: Limiter,
    login_global: Limiter,
    register_ip: Limiter,
    token_ip: Limiter,
}

/// OAuth error (RFC 6749 §5.2 shape).
#[derive(Debug)]
pub struct OAuthError {
    status: StatusCode,
    error: &'static str,
    description: String,
}

impl OAuthError {
    fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            error,
            description: description.into(),
        }
    }
    fn bad(error: &'static str, d: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error, d)
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut r = (
            self.status,
            Json(json!({"error": self.error, "error_description": self.description})),
        )
            .into_response();
        no_store(r.headers_mut());
        if self.status == StatusCode::UNAUTHORIZED {
            r.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic"));
        }
        r
    }
}

fn no_store(h: &mut HeaderMap) {
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

impl OAuth {
    pub fn new(cfg: &Config) -> Self {
        Self {
            file: JsonFile::new(&cfg.data_dir, FILE, LOCK),
            pending: Mutex::new(HashMap::new()),
            codes: Mutex::new(HashMap::new()),
            cimd: Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("secretary-mcp/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            login_ip: Limiter::new(5, Duration::from_secs(300)),
            login_global: Limiter::new(20, Duration::from_secs(900)),
            register_ip: Limiter::new(10, Duration::from_secs(3600)),
            token_ip: Limiter::new(60, Duration::from_secs(60)),
        }
    }

    pub fn data(&self) -> std::io::Result<OAuthData> {
        self.file.read(Clone::clone)
    }

    /// Add `add` to active grants (by grant id or client id) and to their live
    /// access and refresh tokens, so the next refresh keeps them. Returns how many grants.
    pub fn add_scopes(&self, target: &str, add: &Scopes) -> std::io::Result<usize> {
        self.file.update(|d| {
            let mut ids = Vec::new();
            for g in d.grants.iter_mut() {
                if !g.revoked && (g.id == target || g.client_id == target) {
                    g.scopes = Scopes::of(g.scopes.iter().chain(add.iter()));
                    ids.push(g.id.clone());
                }
            }
            for t in d.tokens.iter_mut() {
                if ids.contains(&t.grant_id) {
                    t.scopes = Scopes::of(t.scopes.iter().chain(add.iter()));
                }
            }
            (!ids.is_empty(), ids.len())
        })
    }

    /// Revoke grants by grant id, client id, or `all`. Returns how many.
    pub fn revoke(&self, target: &str) -> std::io::Result<usize> {
        self.file.update(|d| {
            let mut n = 0;
            for g in d.grants.iter_mut() {
                if !g.revoked && (target == "all" || g.id == target || g.client_id == target) {
                    g.revoked = true;
                    n += 1;
                }
            }
            let live: Vec<String> = d
                .grants
                .iter()
                .filter(|g| !g.revoked)
                .map(|g| g.id.clone())
                .collect();
            d.tokens.retain(|t| live.contains(&t.grant_id));
            (n > 0, n)
        })
    }

    /// The caller for a presented `mcpa_…` access token.
    pub fn authenticate(&self, token: &str, resource: &str) -> std::io::Result<Option<Caller>> {
        if !token.starts_with(ACCESS_PREFIX) {
            return Ok(None);
        }
        let hash = sha256_hex(token);
        let now = unix_now();
        self.file.read(|d| {
            let t = d.tokens.iter().find(|t| {
                t.kind == TokenKind::Access && bool::from(t.hash.as_bytes().ct_eq(hash.as_bytes()))
            })?;
            if t.expires_at <= now {
                return None;
            }
            let g = d.grants.iter().find(|g| g.id == t.grant_id)?;
            // Audience: tokens are only good for the resource they were issued for.
            if g.revoked || g.resource != resource {
                return None;
            }
            Some(Caller {
                name: g.caller.clone(),
                kind: CallerKind::OAuth,
                scopes: t.scopes.clone(),
                id: g.id.clone(),
            })
        })
    }

    fn prune(d: &mut OAuthData) {
        let now = unix_now();
        d.tokens.retain(|t| t.expires_at > now);
        let live: Vec<&str> = d.tokens.iter().map(|t| t.grant_id.as_str()).collect();
        let keep: Vec<bool> = d
            .grants
            .iter()
            .map(|g| !g.revoked && live.contains(&g.id.as_str()))
            .collect();
        let mut it = keep.iter();
        d.grants.retain(|_| *it.next().unwrap_or(&false));
    }

    /// CIMD or DCR client by id.
    async fn client(&self, cfg: &Config, client_id: &str) -> Result<ClientInfo, String> {
        if client_id.starts_with("https://") || client_id.starts_with("http://") {
            return self.cimd_client(cfg, client_id).await;
        }
        let found = self
            .file
            .read(|d| d.clients.iter().find(|c| c.client_id == client_id).cloned())
            .map_err(|e| format!("storage error: {e}"))?;
        let c = found.ok_or_else(|| "unknown client_id".to_owned())?;
        Ok(ClientInfo {
            client_id: c.client_id,
            name: c.client_name,
            redirect_uris: c.redirect_uris,
            auth_method: c.auth_method,
            secret_hash: c.secret_hash,
            caller: c.caller,
        })
    }

    /// Fetch and check a client-id metadata document (only under the
    /// configured prefixes, no redirects, ≤ 32 KiB, cached 1 h).
    async fn cimd_client(&self, cfg: &Config, url: &str) -> Result<ClientInfo, String> {
        if !cfg
            .cimd_prefixes
            .iter()
            .any(|p| url.starts_with(p.as_str()) && url.len() > p.len())
        {
            return Err("this client_id metadata URL is not allowed".into());
        }
        let parsed = Url::parse(url).map_err(|_| "client_id is not a valid URL".to_owned())?;
        if parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.as_str() != url {
            return Err("client_id URL must be in canonical form".into());
        }
        if let Some((at, info)) = self.cimd.lock().unwrap_or_else(|p| p.into_inner()).get(url) {
            if at.elapsed() < CIMD_CACHE {
                return Ok(info.clone());
            }
        }
        let resp = self
            .http
            .get(url)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| format!("could not fetch the client metadata: {}", e.without_url()))?;
        if resp.status() != StatusCode::OK {
            return Err(format!("client metadata answered {}", resp.status()));
        }
        if resp
            .content_length()
            .is_some_and(|n| n as usize > CIMD_MAX_BYTES)
        {
            return Err("client metadata too large".into());
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("could not read the client metadata: {}", e.without_url()))?;
        if bytes.len() > CIMD_MAX_BYTES {
            return Err("client metadata too large".into());
        }
        let doc: Value =
            serde_json::from_slice(&bytes).map_err(|_| "client metadata is not JSON".to_owned())?;
        if doc.get("client_id").and_then(Value::as_str) != Some(url) {
            return Err("client metadata client_id does not match its URL".into());
        }
        let redirect_uris: Vec<String> = doc
            .get("redirect_uris")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if redirect_uris.is_empty() {
            return Err("client metadata has no redirect_uris".into());
        }
        if let Some(bad) = redirect_uris
            .iter()
            .find(|u| !redirect_allowed(u, &cfg.redirect_allow))
        {
            return Err(format!("redirect_uri not allowed: {bad}"));
        }
        // We only do public CIMD clients (PKCE). ChatGPT's document lists
        // `none` among its supported methods.
        let method = doc
            .get("token_endpoint_auth_method")
            .and_then(Value::as_str)
            .unwrap_or("none");
        let supports_none = method == "none"
            || doc
                .get("token_endpoint_auth_methods_supported")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|m| m == "none"));
        if !supports_none {
            return Err(format!("unsupported token_endpoint_auth_method {method:?}"));
        }
        let host = parsed.host_str().unwrap_or_default().to_owned();
        let info = ClientInfo {
            client_id: url.to_owned(),
            name: doc
                .get("client_name")
                .and_then(Value::as_str)
                .map(|s| truncate(s, 80))
                .unwrap_or_else(|| host.clone()),
            redirect_uris,
            auth_method: "none".into(),
            secret_hash: None,
            caller: caller_for_host(&host),
        };
        self.cimd
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(url.to_owned(), (Instant::now(), info.clone()));
        Ok(info)
    }

    fn issue_tokens(
        &self,
        cfg: &Config,
        grant: Option<Grant>,
        grant_id: &str,
        scopes: &Scopes,
    ) -> std::io::Result<TokenPair> {
        let access = format!("{ACCESS_PREFIX}{}", random_secret()?);
        let refresh = format!("{REFRESH_PREFIX}{}", random_secret()?);
        let now = unix_now();
        let recs = [
            TokenRec {
                hash: sha256_hex(&access),
                kind: TokenKind::Access,
                grant_id: grant_id.to_owned(),
                scopes: scopes.clone(),
                expires_at: now + cfg.access_ttl.as_secs(),
                used: false,
            },
            TokenRec {
                hash: sha256_hex(&refresh),
                kind: TokenKind::Refresh,
                grant_id: grant_id.to_owned(),
                scopes: scopes.clone(),
                expires_at: now + cfg.refresh_ttl.as_secs(),
                used: false,
            },
        ];
        self.file.update(|d| {
            Self::prune(d);
            if let Some(g) = grant {
                d.grants.push(g);
            }
            d.tokens.extend(recs);
            (true, ())
        })?;
        Ok(TokenPair {
            access,
            refresh,
            expires_in: cfg.access_ttl.as_secs(),
            scope: scopes.to_oauth(),
        })
    }

    /// Redeem a validated authorization code: mark it redeemed and write the
    /// grant and its tokens in ONE critical section under the `codes` lock.
    /// A replay can then never see a redeemed code whose grant is not in
    /// `oauth.json` yet (its `revoke_grant` used to be a no-op in that
    /// window), and a request that loses the race to a concurrent
    /// redemption is a replay too: the grant is revoked.
    /// (verification/models/src/oauth.rs, formal/oauth/OAuth.tla, bug O1.)
    fn redeem_code(
        &self,
        cfg: &Config,
        code_hash: &str,
        grant: Grant,
        scopes: &Scopes,
    ) -> Result<TokenPair, Redeem> {
        let mut codes = self.codes.lock().unwrap_or_else(|p| p.into_inner());
        match codes.get_mut(code_hash) {
            Some(c) if c.redeemed.is_none() => {
                let grant_id = grant.id.clone();
                let pair = self
                    .issue_tokens(cfg, Some(grant), &grant_id, scopes)
                    .map_err(Redeem::Io)?;
                c.redeemed = Some(grant_id);
                Ok(pair)
            }
            Some(c) => {
                let replayed = c.redeemed.clone().unwrap_or_default();
                drop(codes);
                self.revoke_grant(&replayed);
                Err(Redeem::Replay(replayed))
            }
            None => Err(Redeem::Gone),
        }
    }

    /// Rotate a refresh token: mark it used if it still is unused, then issue
    /// the new pair. Losing that race to a concurrent use of the same token is
    /// a reuse like any other: the grant is revoked (it used to answer
    /// "already used" and keep the grant). (Bug O2.)
    fn rotate_refresh(
        &self,
        cfg: &Config,
        refresh_hash: &str,
        grant_id: &str,
        scopes: &Scopes,
    ) -> Result<TokenPair, Redeem> {
        let rotated = self
            .file
            .update(|d| {
                match d
                    .tokens
                    .iter_mut()
                    .find(|x| x.hash == refresh_hash && !x.used)
                {
                    Some(x) => {
                        x.used = true;
                        (true, true)
                    }
                    None => (false, false),
                }
            })
            .map_err(Redeem::Io)?;
        if !rotated {
            self.revoke_grant(grant_id);
            return Err(Redeem::Replay(grant_id.to_owned()));
        }
        self.issue_tokens(cfg, None, grant_id, scopes)
            .map_err(Redeem::Io)
    }

    fn revoke_grant(&self, grant_id: &str) {
        let _ = self.file.update(|d| {
            for g in d.grants.iter_mut().filter(|g| g.id == grant_id) {
                g.revoked = true;
            }
            d.tokens.retain(|t| t.grant_id != grant_id);
            (true, ())
        });
    }
}

/// Why a code redemption or a refresh rotation did not issue tokens.
#[derive(Debug)]
enum Redeem {
    /// Already redeemed / rotated: the grant (named) has been revoked.
    Replay(String),
    /// The code is no longer in memory (burned by a failed verifier, expired).
    Gone,
    Io(std::io::Error),
}

struct TokenPair {
    access: String,
    refresh: String,
    expires_in: u64,
    scope: String,
}

impl TokenPair {
    fn into_response(self) -> Response {
        let mut r = Json(json!({
            "access_token": self.access,
            "token_type": "Bearer",
            "expires_in": self.expires_in,
            "refresh_token": self.refresh,
            "scope": self.scope,
        }))
        .into_response();
        no_store(r.headers_mut());
        r
    }
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// The caller name Lucina sees for an OAuth client, from its host.
pub fn caller_for_host(host: &str) -> String {
    let h = host.to_ascii_lowercase();
    let is = |d: &str| h == d || h.ends_with(&format!(".{d}"));
    if is("chatgpt.com") || is("openai.com") {
        return "chatgpt".into();
    }
    if is("claude.ai") || is("claude.com") || is("anthropic.com") {
        return "claude".into();
    }
    if is("x.ai") || is("grok.com") {
        return "grok".into();
    }
    if is("cursor.com") || is("cursor.sh") {
        return "cursor".into();
    }
    let label: String = h
        .split('.')
        .next()
        .unwrap_or("oauth")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(32)
        .collect();
    if label.is_empty() {
        "oauth".into()
    } else {
        label
    }
}

/// `uri` is https, has no fragment or userinfo, and is allowed: equal to an
/// exact entry, or below an entry that ends in `/`.
pub fn redirect_allowed(uri: &str, allow: &[String]) -> bool {
    let Ok(u) = Url::parse(uri) else {
        return false;
    };
    if u.scheme() != "https"
        || u.fragment().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.host_str().is_none()
        || u.as_str() != uri
    {
        return false;
    }
    allow.iter().any(|a| {
        if a.ends_with('/') {
            uri.len() > a.len() && uri.starts_with(a.as_str())
        } else {
            uri == a
        }
    })
}

fn resource_ok(cfg: &Config, r: Option<&str>) -> bool {
    match r {
        None => true,
        Some(r) => {
            let want = cfg.resource();
            r == want || r.strip_suffix('/') == Some(want.as_str())
        }
    }
}

// -- metadata -------------------------------------------------------------------

pub async fn protected_resource(State(st): State<AppState>) -> Response {
    let c = &st.cfg;
    let mut r = Json(json!({
        "resource": c.resource(),
        "authorization_servers": [c.public_url],
        "scopes_supported": Scope::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "bearer_methods_supported": ["header"],
        "resource_name": "Lucina MCP",
        "resource_documentation": c.url("/mcp"),
    }))
    .into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    r
}

pub async fn authorization_server(State(st): State<AppState>) -> Response {
    let c = &st.cfg;
    let mut m = json!({
        "issuer": c.public_url,
        "authorization_endpoint": c.url("/oauth/authorize"),
        "token_endpoint": c.url("/oauth/token"),
        "registration_endpoint": c.url("/oauth/register"),
        "revocation_endpoint": c.url("/oauth/revoke"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "revocation_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "scopes_supported": Scope::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "authorization_response_iss_parameter_supported": true,
        "client_id_metadata_document_supported": !c.cimd_prefixes.is_empty(),
        "service_documentation": c.url("/mcp"),
    });
    if c.cimd_prefixes.is_empty() {
        m.as_object_mut()
            .map(|o| o.remove("client_id_metadata_document_supported"));
    }
    let mut r = Json(m).into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    r
}

// -- registration (RFC 7591) ----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
    #[serde(default)]
    application_type: Option<String>,
}

pub async fn register(
    State(st): State<AppState>,
    headers: HeaderMap,
    connect: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
    body: axum::body::Bytes,
) -> Result<Response, OAuthError> {
    let ip = crate::auth::client_ip(st.cfg.trust_proxy, &headers, connect.map(|c| c.0 .0));
    st.oauth.register_ip.check(&ip).map_err(|_| {
        OAuthError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many registrations",
        )
    })?;
    let req: RegisterRequest = serde_json::from_slice(&body)
        .map_err(|e| OAuthError::bad("invalid_client_metadata", format!("bad JSON: {e}")))?;
    if req.redirect_uris.is_empty() || req.redirect_uris.len() > 5 {
        return Err(OAuthError::bad(
            "invalid_redirect_uri",
            "1-5 redirect_uris are required",
        ));
    }
    if let Some(bad) = req
        .redirect_uris
        .iter()
        .find(|u| !redirect_allowed(u, &st.cfg.redirect_allow))
    {
        tracing::warn!(target: "secretary_mcp::audit", event = "oauth_register_refused", "redirect_uri not allowed");
        return Err(OAuthError::bad(
            "invalid_redirect_uri",
            format!("redirect_uri not allowed (https and an allowed callback only): {bad}"),
        ));
    }
    let method = req
        .token_endpoint_auth_method
        .as_deref()
        .unwrap_or("client_secret_basic");
    if !matches!(
        method,
        "none" | "client_secret_post" | "client_secret_basic"
    ) {
        return Err(OAuthError::bad(
            "invalid_client_metadata",
            "token_endpoint_auth_method must be none, client_secret_post or client_secret_basic",
        ));
    }
    let grant_types = req
        .grant_types
        .unwrap_or_else(|| vec!["authorization_code".into(), "refresh_token".into()]);
    if grant_types.is_empty()
        || !grant_types
            .iter()
            .all(|g| g == "authorization_code" || g == "refresh_token")
    {
        return Err(OAuthError::bad(
            "invalid_client_metadata",
            "grant_types may only be authorization_code and refresh_token",
        ));
    }
    let response_types = req.response_types.unwrap_or_else(|| vec!["code".into()]);
    if response_types.iter().any(|r| r != "code") {
        return Err(OAuthError::bad(
            "invalid_client_metadata",
            "response_types may only be code",
        ));
    }
    if req
        .application_type
        .as_deref()
        .is_some_and(|a| a != "web" && a != "native")
    {
        return Err(OAuthError::bad(
            "invalid_client_metadata",
            "application_type must be web or native",
        ));
    }
    let host = Url::parse(&req.redirect_uris[0])
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let name = req
        .client_name
        .as_deref()
        .map(|n| truncate(n, 80))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| host.clone());
    let internal = |e: std::io::Error| {
        OAuthError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            e.to_string(),
        )
    };
    let client_id = random_id("mcpc_").map_err(internal)?;
    let secret = if method == "none" {
        None
    } else {
        Some(random_secret().map_err(internal)?)
    };
    let now = unix_now();
    let rec = RegisteredClient {
        client_id: client_id.clone(),
        secret_hash: secret.as_deref().map(sha256_hex),
        auth_method: method.to_owned(),
        client_name: name.clone(),
        redirect_uris: req.redirect_uris.clone(),
        caller: caller_for_host(&host),
        created_at: now,
    };
    st.oauth
        .file
        .update(|d| {
            d.clients.push(rec);
            if d.clients.len() > MAX_DCR_CLIENTS {
                // Drop the oldest clients that hold no live grant.
                let used: Vec<String> = d
                    .grants
                    .iter()
                    .filter(|g| !g.revoked)
                    .map(|g| g.client_id.clone())
                    .collect();
                let mut excess = d.clients.len() - MAX_DCR_CLIENTS;
                d.clients.retain(|c| {
                    if excess > 0 && !used.contains(&c.client_id) {
                        excess -= 1;
                        false
                    } else {
                        true
                    }
                });
            }
            (true, ())
        })
        .map_err(internal)?;
    tracing::info!(target: "secretary_mcp::audit", event = "oauth_register", client = %client_id, method, "client registered");
    let mut out = json!({
        "client_id": client_id,
        "client_id_issued_at": now,
        "client_name": name,
        "redirect_uris": req.redirect_uris,
        "token_endpoint_auth_method": method,
        "grant_types": grant_types,
        "response_types": response_types,
    });
    if let Some(s) = secret {
        out["client_secret"] = json!(s);
        out["client_secret_expires_at"] = json!(0);
    }
    let mut r = (StatusCode::CREATED, Json(out)).into_response();
    no_store(r.headers_mut());
    Ok(r)
}

// -- authorization endpoint -----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    scope: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
}

/// Redirect back to the client with `params` plus `state` and `iss`.
fn redirect_back(
    cfg: &Config,
    redirect_uri: &str,
    state: Option<&str>,
    params: &[(&str, &str)],
) -> Response {
    let mut u = match Url::parse(redirect_uri) {
        Ok(u) => u,
        Err(_) => return login::error_page("Invalid redirect URI.").into_response(),
    };
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in params {
            q.append_pair(k, v);
        }
        if let Some(s) = state {
            q.append_pair("state", s);
        }
        q.append_pair("iss", &cfg.public_url);
    }
    let mut r = Redirect::to(u.as_str()).into_response();
    no_store(r.headers_mut());
    r
}

pub async fn authorize_get(
    State(st): State<AppState>,
    Query(q): Query<AuthorizeQuery>,
) -> Response {
    let cfg = &st.cfg;
    // 1. client and redirect_uri: on failure never redirect (RFC 6749 §4.1.2.1).
    let Some(client_id) = q
        .client_id
        .as_deref()
        .filter(|s| !s.is_empty() && s.len() <= 512)
    else {
        return login::error_page("Missing client_id.").into_response();
    };
    let client = match st.oauth.client(cfg, client_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "secretary_mcp::audit", event = "oauth_authorize_refused", reason = %e);
            return login::error_page(&format!("Unknown or refused client: {e}.")).into_response();
        }
    };
    let redirect_uri = match q.redirect_uri.as_deref() {
        Some(r)
            if client.redirect_uris.iter().any(|u| u == r)
                && redirect_allowed(r, &cfg.redirect_allow) =>
        {
            r.to_owned()
        }
        Some(_) => {
            return login::error_page(
                "redirect_uri does not match the client's registered redirect URIs.",
            )
            .into_response()
        }
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        None => return login::error_page("redirect_uri is required.").into_response(),
    };
    let state = q.state.as_deref().filter(|s| s.len() <= 1024);
    let fail = |error: &str, desc: &str| {
        redirect_back(
            cfg,
            &redirect_uri,
            state,
            &[("error", error), ("error_description", desc)],
        )
    };
    // 2. everything else: report to the client.
    if q.response_type.as_deref() != Some("code") {
        return fail(
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    let Some(challenge) = q.code_challenge.as_deref() else {
        return fail("invalid_request", "PKCE code_challenge is required");
    };
    if q.code_challenge_method.as_deref() != Some("S256") {
        return fail("invalid_request", "code_challenge_method must be S256");
    }
    if challenge.len() != 43
        || !challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return fail(
            "invalid_request",
            "code_challenge must be base64url(SHA-256), 43 characters",
        );
    }
    if !resource_ok(cfg, q.resource.as_deref()) {
        return fail("invalid_target", "resource must be this server's /mcp URL");
    }
    let mut scopes = match q.scope.as_deref() {
        Some(s) => Scopes::parse_lenient(s),
        None => Scopes::all(),
    };
    if scopes.is_empty() {
        scopes = Scopes::all();
    }
    let request_id = match random_secret() {
        Ok(id) => id,
        Err(_) => return fail("server_error", "no randomness"),
    };
    let pending = Pending {
        client,
        redirect_uri,
        state: state.map(str::to_owned),
        code_challenge: challenge.to_owned(),
        scopes,
        resource: cfg.resource(),
        created: Instant::now(),
    };
    let page = login::consent_page(
        &request_id,
        &pending.client.name,
        &pending.redirect_uri,
        &pending.scopes,
        None,
    );
    {
        let mut p = st.oauth.pending.lock().unwrap_or_else(|p| p.into_inner());
        p.retain(|_, v| v.created.elapsed() < PENDING_TTL);
        // Bounded: a flood of authorize requests evicts the oldest ones
        // instead of locking the owner out.
        while p.len() >= 200 {
            let Some(oldest) = p
                .iter()
                .min_by_key(|(_, v)| v.created)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            p.remove(&oldest);
        }
        p.insert(request_id, pending);
    }
    page.into_response()
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeForm {
    request_id: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    decision: String,
    #[serde(default)]
    scope_read: Option<String>,
    #[serde(default)]
    scope_chat: Option<String>,
    #[serde(default)]
    scope_write: Option<String>,
    #[serde(default)]
    scope_computer: Option<String>,
}

pub async fn authorize_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    connect: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
    Form(f): Form<AuthorizeForm>,
) -> Response {
    let cfg = &st.cfg;
    let pending = {
        let p = st.oauth.pending.lock().unwrap_or_else(|p| p.into_inner());
        p.get(&f.request_id)
            .filter(|v| v.created.elapsed() < PENDING_TTL)
            .cloned()
    };
    let Some(pending) = pending else {
        return login::error_page("This sign-in request expired. Start again from your app.")
            .into_response();
    };
    let take = || {
        st.oauth
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&f.request_id)
    };
    let again = |msg: &str| {
        login::consent_page(
            &f.request_id,
            &pending.client.name,
            &pending.redirect_uri,
            &pending.scopes,
            Some(msg),
        )
        .into_response()
    };
    if f.decision != "approve" {
        take();
        tracing::info!(target: "secretary_mcp::audit", event = "oauth_denied", client = %pending.client.client_id);
        return redirect_back(
            cfg,
            &pending.redirect_uri,
            pending.state.as_deref(),
            &[
                ("error", "access_denied"),
                ("error_description", "the owner declined"),
            ],
        );
    }
    let picked = Scopes::of(
        [
            (Scope::Read, &f.scope_read),
            (Scope::Chat, &f.scope_chat),
            (Scope::Write, &f.scope_write),
            (Scope::Computer, &f.scope_computer),
        ]
        .into_iter()
        .filter(|(_, v)| v.is_some())
        .map(|(s, _)| s),
    )
    .intersect(&pending.scopes);
    if picked.is_empty() {
        return again("Pick at least one permission.");
    }
    if f.email.trim().is_empty()
        || f.password.is_empty()
        || f.password.len() > 1024
        || f.email.len() > 254
    {
        return again("Enter your Secretary email and password.");
    }
    let ip = crate::auth::client_ip(cfg.trust_proxy, &headers, connect.map(|c| c.0 .0));
    if let Err(wait) = st
        .oauth
        .login_ip
        .check(&ip)
        .and_then(|_| st.oauth.login_global.check("all"))
    {
        tracing::warn!(target: "secretary_mcp::audit", event = "oauth_login_rate_limited");
        return again(&format!(
            "Too many sign-in attempts. Try again in {wait} s."
        ));
    }
    match st
        .backend
        .check_owner(f.email.trim(), &f.password, &ip)
        .await
    {
        Ok(OwnerCheck::Ok) => {}
        Ok(OwnerCheck::Invalid) => {
            tracing::warn!(target: "secretary_mcp::audit", event = "oauth_login_failed", client = %pending.client.client_id);
            return again("Wrong email or password.");
        }
        Ok(OwnerCheck::Locked(s)) => {
            tracing::warn!(target: "secretary_mcp::audit", event = "oauth_login_locked");
            return again(&format!("Password sign-in is locked. Try again in {s} s."));
        }
        Err(e) => {
            tracing::error!(target: "secretary_mcp::audit", event = "oauth_login_backend_error", code = %e.code, status = e.status);
            return again("Secretary is not reachable right now. Try again in a minute.");
        }
    }
    let Some(pending) = take() else {
        return login::error_page("This sign-in request was already used.").into_response();
    };
    let code = match random_secret() {
        Ok(c) => c,
        Err(_) => return login::error_page("Server error.").into_response(),
    };
    {
        let mut codes = st.oauth.codes.lock().unwrap_or_else(|p| p.into_inner());
        codes.retain(|_, c| c.created.elapsed() < CODE_TTL * 2);
        codes.insert(
            sha256_hex(&code),
            CodeRec {
                client_id: pending.client.client_id.clone(),
                client_name: pending.client.name.clone(),
                caller: pending.client.caller.clone(),
                redirect_uri: pending.redirect_uri.clone(),
                code_challenge: pending.code_challenge.clone(),
                scopes: picked.clone(),
                resource: pending.resource.clone(),
                created: Instant::now(),
                redeemed: None,
            },
        );
    }
    tracing::info!(target: "secretary_mcp::audit", event = "oauth_consent", client = %pending.client.client_id, caller = %pending.client.caller, scopes = %picked);
    redirect_back(
        cfg,
        &pending.redirect_uri,
        pending.state.as_deref(),
        &[("code", &code)],
    )
}

// -- token endpoint ---------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
    // RFC 7009
    token: Option<String>,
}

/// Client authentication: Basic header, form secret, or public client.
async fn authenticate_client(
    st: &AppState,
    headers: &HeaderMap,
    f: &TokenForm,
) -> Result<ClientInfo, OAuthError> {
    let invalid = |d: &str| OAuthError::new(StatusCode::UNAUTHORIZED, "invalid_client", d);
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| STANDARD.decode(b.trim()).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| {
            let (id, secret) = s.split_once(':')?;
            Some((form_decode(id), form_decode(secret)))
        });
    let (client_id, secret, used_basic) = match (&basic, &f.client_id) {
        (Some((id, s)), form_id) => {
            if form_id.as_deref().is_some_and(|f| f != id) {
                return Err(invalid("client_id mismatch"));
            }
            (id.clone(), Some(s.clone()), true)
        }
        (None, Some(id)) => (id.clone(), f.client_secret.clone(), false),
        (None, None) => return Err(invalid("client_id is required")),
    };
    let client = st
        .oauth
        .client(&st.cfg, &client_id)
        .await
        .map_err(|e| invalid(&e))?;
    match client.auth_method.as_str() {
        "none" => {
            // A CIMD client may send a private_key_jwt assertion; it is a
            // public client to us (PKCE binds the code), so it is ignored.
            if secret.is_some() {
                return Err(invalid("public client must not send a client_secret"));
            }
        }
        m => {
            let Some(secret) = secret else {
                return Err(invalid("client authentication required"));
            };
            if (m == "client_secret_basic") != used_basic {
                return Err(invalid("wrong client authentication method"));
            }
            let want = client.secret_hash.as_deref().unwrap_or_default();
            if !bool::from(sha256_hex(&secret).as_bytes().ct_eq(want.as_bytes())) {
                return Err(invalid("bad client credentials"));
            }
        }
    }
    Ok(client)
}

/// `application/x-www-form-urlencoded` decoding of one Basic-auth part
/// (RFC 6749 §2.3.1).
fn form_decode(s: &str) -> String {
    url_form_pairs(&format!("x={s}"))
        .into_iter()
        .next()
        .map(|(_, v)| v)
        .unwrap_or_default()
}

fn url_form_pairs(s: &str) -> Vec<(String, String)> {
    Url::parse(&format!("http://x/?{s}"))
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn verifier_ok(v: &str) -> bool {
    (43..=128).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

pub async fn token(
    State(st): State<AppState>,
    headers: HeaderMap,
    connect: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
    Form(f): Form<TokenForm>,
) -> Result<Response, OAuthError> {
    let ip = crate::auth::client_ip(st.cfg.trust_proxy, &headers, connect.map(|c| c.0 .0));
    st.oauth.token_ip.check(&ip).map_err(|_| {
        OAuthError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many token requests",
        )
    })?;
    let internal = |e: std::io::Error| {
        OAuthError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            e.to_string(),
        )
    };
    match f.grant_type.as_deref() {
        Some("authorization_code") => {
            let client = authenticate_client(&st, &headers, &f).await?;
            let code = f
                .code
                .as_deref()
                .ok_or_else(|| OAuthError::bad("invalid_request", "code is required"))?;
            let verifier = f.code_verifier.as_deref().ok_or_else(|| {
                OAuthError::bad("invalid_request", "code_verifier is required (PKCE)")
            })?;
            let hash = sha256_hex(code);
            let rec = {
                let mut codes = st.oauth.codes.lock().unwrap_or_else(|p| p.into_inner());
                match codes.get_mut(&hash) {
                    None => None,
                    Some(c) if c.created.elapsed() >= CODE_TTL => None,
                    Some(c) => {
                        if let Some(g) = c.redeemed.clone() {
                            Some(Err(g))
                        } else {
                            Some(Ok(c.clone()))
                        }
                    }
                }
            };
            let rec = match rec {
                None => return Err(OAuthError::bad("invalid_grant", "unknown or expired code")),
                Some(Err(grant_id)) => {
                    st.oauth.revoke_grant(&grant_id);
                    tracing::warn!(target: "secretary_mcp::audit", event = "oauth_code_replay", grant = %grant_id, "grant revoked");
                    return Err(OAuthError::bad("invalid_grant", "code already used"));
                }
                Some(Ok(r)) => r,
            };
            if rec.client_id != client.client_id {
                return Err(OAuthError::bad(
                    "invalid_grant",
                    "code was issued to another client",
                ));
            }
            if f.redirect_uri
                .as_deref()
                .is_some_and(|r| r != rec.redirect_uri)
            {
                return Err(OAuthError::bad(
                    "invalid_grant",
                    "redirect_uri does not match",
                ));
            }
            if !verifier_ok(verifier) || sha256_b64url(verifier) != rec.code_challenge {
                tracing::warn!(target: "secretary_mcp::audit", event = "oauth_pkce_failed", client = %client.client_id);
                // A failed verifier burns the code.
                st.oauth
                    .codes
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&hash);
                return Err(OAuthError::bad("invalid_grant", "PKCE verification failed"));
            }
            if !resource_ok(&st.cfg, f.resource.as_deref()) {
                return Err(OAuthError::bad(
                    "invalid_target",
                    "resource must be this server's /mcp URL",
                ));
            }
            let grant = Grant {
                id: random_id("grant_").map_err(internal)?,
                client_id: rec.client_id.clone(),
                client_name: rec.client_name.clone(),
                caller: rec.caller.clone(),
                scopes: rec.scopes.clone(),
                resource: rec.resource.clone(),
                created_at: unix_now(),
                revoked: false,
            };
            let grant_id = grant.id.clone();
            let pair = match st.oauth.redeem_code(&st.cfg, &hash, grant, &rec.scopes) {
                Ok(p) => p,
                Err(Redeem::Io(e)) => return Err(internal(e)),
                Err(Redeem::Replay(g)) => {
                    tracing::warn!(target: "secretary_mcp::audit", event = "oauth_code_replay", grant = %g, "grant revoked (concurrent redemption)");
                    return Err(OAuthError::bad("invalid_grant", "code already used"));
                }
                Err(Redeem::Gone) => {
                    return Err(OAuthError::bad("invalid_grant", "code already used"))
                }
            };
            tracing::info!(target: "secretary_mcp::audit", event = "oauth_token", grant = %grant_id, caller = %rec.caller, "authorization_code");
            Ok(pair.into_response())
        }
        Some("refresh_token") => {
            let client = authenticate_client(&st, &headers, &f).await?;
            let rt = f
                .refresh_token
                .as_deref()
                .ok_or_else(|| OAuthError::bad("invalid_request", "refresh_token is required"))?;
            let hash = sha256_hex(rt);
            let now = unix_now();
            enum Found {
                Missing,
                Replay(String),
                Ok(Box<(TokenRec, Grant)>),
            }
            let found = st
                .oauth
                .file
                .read(|d| {
                    let Some(t) = d
                        .tokens
                        .iter()
                        .find(|t| t.kind == TokenKind::Refresh && t.hash == hash)
                    else {
                        return Found::Missing;
                    };
                    if t.used {
                        return Found::Replay(t.grant_id.clone());
                    }
                    match d.grants.iter().find(|g| g.id == t.grant_id) {
                        Some(g) if !g.revoked && t.expires_at > now => {
                            Found::Ok(Box::new((t.clone(), g.clone())))
                        }
                        _ => Found::Missing,
                    }
                })
                .map_err(internal)?;
            let (t, g) = match found {
                Found::Missing => {
                    return Err(OAuthError::bad(
                        "invalid_grant",
                        "unknown, expired or revoked refresh token",
                    ))
                }
                Found::Replay(grant_id) => {
                    st.oauth.revoke_grant(&grant_id);
                    tracing::warn!(target: "secretary_mcp::audit", event = "oauth_refresh_replay", grant = %grant_id, "grant revoked");
                    return Err(OAuthError::bad(
                        "invalid_grant",
                        "refresh token already used; the grant is revoked",
                    ));
                }
                Found::Ok(b) => *b,
            };
            if g.client_id != client.client_id {
                return Err(OAuthError::bad(
                    "invalid_grant",
                    "refresh token was issued to another client",
                ));
            }
            if !resource_ok(&st.cfg, f.resource.as_deref()) {
                return Err(OAuthError::bad(
                    "invalid_target",
                    "resource must be this server's /mcp URL",
                ));
            }
            let scopes = match f.scope.as_deref() {
                Some(s) => {
                    let want = Scopes::parse_lenient(s);
                    if want.is_empty() || !want.is_subset_of(&t.scopes) {
                        return Err(OAuthError::bad(
                            "invalid_scope",
                            "scope must be a subset of the granted scopes",
                        ));
                    }
                    want
                }
                None => t.scopes.clone(),
            };
            // Rotate: mark the presented token used (kept for replay detection).
            let pair = match st.oauth.rotate_refresh(&st.cfg, &hash, &g.id, &scopes) {
                Ok(p) => p,
                Err(Redeem::Io(e)) => return Err(internal(e)),
                Err(_) => {
                    tracing::warn!(target: "secretary_mcp::audit", event = "oauth_refresh_replay", grant = %g.id, "grant revoked (concurrent refresh)");
                    return Err(OAuthError::bad(
                        "invalid_grant",
                        "refresh token already used; the grant is revoked",
                    ));
                }
            };
            tracing::info!(target: "secretary_mcp::audit", event = "oauth_token", grant = %g.id, caller = %g.caller, "refresh_token");
            Ok(pair.into_response())
        }
        Some(_) => Err(OAuthError::bad(
            "unsupported_grant_type",
            "use authorization_code or refresh_token",
        )),
        None => Err(OAuthError::bad("invalid_request", "grant_type is required")),
    }
}

/// RFC 7009 revocation: always 200 for a valid client.
pub async fn revoke(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<TokenForm>,
) -> Result<Response, OAuthError> {
    let client = authenticate_client(&st, &headers, &f).await?;
    if let Some(tok) = f.token.as_deref() {
        let hash = sha256_hex(tok);
        let grant = st
            .oauth
            .file
            .read(|d| {
                let t = d.tokens.iter().find(|t| t.hash == hash)?;
                d.grants
                    .iter()
                    .find(|g| g.id == t.grant_id && g.client_id == client.client_id)
                    .map(|g| g.id.clone())
            })
            .ok()
            .flatten();
        if let Some(g) = grant {
            st.oauth.revoke_grant(&g);
            tracing::info!(target: "secretary_mcp::audit", event = "oauth_revoke", grant = %g);
        }
    }
    let mut r = StatusCode::OK.into_response();
    no_store(r.headers_mut());
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow() -> Vec<String> {
        crate::config::CHATGPT_REDIRECTS
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    #[test]
    fn redirect_rules() {
        let a = allow();
        assert!(redirect_allowed(
            "https://chatgpt.com/connector_platform_oauth_redirect",
            &a
        ));
        assert!(redirect_allowed(
            "https://chatgpt.com/connector/oauth/abc123",
            &a
        ));
        for bad in [
            "http://chatgpt.com/connector_platform_oauth_redirect",
            "https://chatgpt.com/connector_platform_oauth_redirect/x",
            "https://chatgpt.com/connector/oauth/",
            "https://evil.com/connector/oauth/x",
            "https://chatgpt.com.evil.com/connector/oauth/x",
            "https://user@chatgpt.com/connector/oauth/x",
            "https://chatgpt.com/connector/oauth/x#frag",
            "https://CHATGPT.com/connector/oauth/x",
            "not a url",
        ] {
            assert!(!redirect_allowed(bad, &a), "{bad}");
        }
    }

    #[test]
    fn callers() {
        assert_eq!(caller_for_host("chatgpt.com"), "chatgpt");
        assert_eq!(caller_for_host("platform.openai.com"), "chatgpt");
        assert_eq!(caller_for_host("example.org"), "example");
        assert_eq!(caller_for_host("www.cursor.com"), "cursor");
        assert_eq!(caller_for_host("cursor.sh"), "cursor");
    }

    // -- token endpoint races (docs/verification.md, bugs O1 and O2) ---------

    fn oauth_in(dir: &std::path::Path) -> (Config, OAuth) {
        let cfg = Config::new("https://getsecretary.tech", dir).unwrap();
        let o = OAuth::new(&cfg);
        (cfg, o)
    }

    fn grant(id: &str) -> Grant {
        Grant {
            id: id.into(),
            client_id: "client".into(),
            client_name: "Client".into(),
            caller: "client".into(),
            scopes: Scopes::all(),
            resource: "res".into(),
            created_at: 0,
            revoked: false,
        }
    }

    fn pending_code(o: &OAuth, hash: &str) {
        o.codes.lock().unwrap().insert(
            hash.into(),
            CodeRec {
                client_id: "client".into(),
                client_name: "Client".into(),
                caller: "client".into(),
                redirect_uri: "https://chatgpt.com/connector_platform_oauth_redirect".into(),
                code_challenge: "x".into(),
                scopes: Scopes::all(),
                resource: "res".into(),
                created: Instant::now(),
                redeemed: None,
            },
        );
    }

    /// Two token requests with the same code both pass the first lookup; the
    /// one that commits second is a replay and must revoke the grant. Before
    /// the fix it answered "code already used" and the grant stayed live, and
    /// a replay landing between "redeemed" and `issue_tokens` revoked nothing
    /// (the grant was not in oauth.json yet). Model: formal/oauth/OAuth.tla.
    #[test]
    fn concurrent_code_redemption_revokes_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, o) = oauth_in(dir.path());
        pending_code(&o, "code-hash");
        let first = o
            .redeem_code(&cfg, "code-hash", grant("grant_a"), &Scopes::all())
            .expect("first redemption issues tokens");
        assert!(o.authenticate(&first.access, "res").unwrap().is_some());
        let second = o.redeem_code(&cfg, "code-hash", grant("grant_b"), &Scopes::all());
        match second {
            Err(Redeem::Replay(g)) => assert_eq!(g, "grant_a"),
            Err(e) => panic!("expected a replay, got {e:?}"),
            Ok(_) => panic!("a second redemption of one code issued tokens"),
        }
        let d = o.data().unwrap();
        assert!(d.grants.iter().any(|g| g.id == "grant_a" && g.revoked));
        assert!(d.grants.iter().all(|g| g.id != "grant_b"));
        assert!(o.authenticate(&first.access, "res").unwrap().is_none());
    }

    /// The redeemed mark and the grant are written together: whenever a code
    /// is marked redeemed, its grant is already in oauth.json.
    #[test]
    fn a_redeemed_code_always_has_its_grant_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, o) = oauth_in(dir.path());
        pending_code(&o, "code-hash");
        o.redeem_code(&cfg, "code-hash", grant("grant_a"), &Scopes::all())
            .unwrap();
        let redeemed = o.codes.lock().unwrap()["code-hash"]
            .redeemed
            .clone()
            .unwrap();
        assert!(o.data().unwrap().grants.iter().any(|g| g.id == redeemed));
        // A burned (forgotten) code is not a replay of anything.
        assert!(matches!(
            o.redeem_code(&cfg, "other", grant("grant_c"), &Scopes::all()),
            Err(Redeem::Gone)
        ));
    }

    /// Two refreshes with the same refresh token both read it as unused; the
    /// one that loses the rotation is a reuse: the grant is revoked (it used
    /// to answer "already used" and keep the grant live).
    #[test]
    fn concurrent_refresh_with_one_token_revokes_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, o) = oauth_in(dir.path());
        pending_code(&o, "code-hash");
        let pair = o
            .redeem_code(&cfg, "code-hash", grant("grant_a"), &Scopes::all())
            .unwrap();
        let rt = sha256_hex(&pair.refresh);
        let rotated = o
            .rotate_refresh(&cfg, &rt, "grant_a", &Scopes::all())
            .expect("the first rotation wins");
        assert!(o.authenticate(&rotated.access, "res").unwrap().is_some());
        let lost = o.rotate_refresh(&cfg, &rt, "grant_a", &Scopes::all());
        match lost {
            Err(Redeem::Replay(g)) => assert_eq!(g, "grant_a"),
            Err(e) => panic!("expected a reuse, got {e:?}"),
            Ok(_) => panic!("one refresh token rotated twice"),
        }
        assert!(o
            .data()
            .unwrap()
            .grants
            .iter()
            .any(|g| g.id == "grant_a" && g.revoked));
        assert!(o.authenticate(&rotated.access, "res").unwrap().is_none());
    }

    #[test]
    fn verifier_rules() {
        assert!(verifier_ok(&"a".repeat(43)));
        assert!(!verifier_ok(&"a".repeat(42)));
        assert!(!verifier_ok(&format!("{}+", "a".repeat(43))));
        assert_eq!(form_decode("a%3Ab+c"), "a:b c");
    }
}
