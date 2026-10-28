//! `/v1/agents/*`: Lucina Agents (lucina-agentd on nezopt) for the apps, Secretary
//! Code and the MCP. The contract is `docs/design/agents-api.md`.
//!
//! The way there: this server → Lucina (`SECRETARY_LUCINA_URL`, the Link tunnel on
//! p1) `/api/secretary/agentd/<daemon path>` → lucina-server forwards to the daemon's
//! unix socket. Bodies, query strings, status codes and errors pass through; responses
//! stream (SSE, long-polls, `wait`).
//!
//! Who is calling decides `X-Lucina-Origin`, which this server always sets itself (a
//! client's own value is never forwarded):
//!
//! | Caller | Credential | Origin |
//! |---|---|---|
//! | an owner device | device token or web session (cookie + CSRF on writes) | `device:<deviceId>` |
//! | the MCP | its agents token (`SECRETARY_MCP_AGENTS_TOKEN_FILE`) with `X-Lucina-Mcp-Caller: <tokenId>:<name>` | `mcp:<tokenId>` (`mcp:unknown` without a valid header) |
//! | the MCP, other ways in | a device token of a device in `SECRETARY_MCP_DEVICE_IDS`, or any device token with `X-Secretary-Caller: mcp:<name>` | `mcp:<name>` (these only lower a caller's rights) |
//! | the Infisical broker | `SECRETARY_AGENTS_BROKER_TOKEN_FILE` token | `service:infisical-broker`, only `POST /v1/agents/permissions` and `GET /v1/agents/permissions/{id}` |
//!
//! The server builds the request to Lucina itself, so no client-sent `X-Lucina-*` header
//! reaches the daemon; the MCP's `X-Lucina-Delegation-Depth` (`0` or `1`) is passed on.
//! An MCP origin never answers a permission request (refused here, and again by the
//! daemon's permission gate); the daemon also clears or refuses what the contract
//! keeps from MCP callers. Lucina's own service token is refused here.
//!
//! Paths: `/v1/agents` and `/v1/agents/{id}…` are the daemon's `/agents…`;
//! `/v1/agents/{permissions,workspaces,projects,clone,providers,stream,commands,files,health,schedules,usage,terminals}…`
//! are the daemon's top-level routes; `/v1/agents/items` and `/v1/agents/search` are `/agents/items` and
//! `/agents/search`.
//!
//! Phase 5: `/v1/agents/terminals/{id}/stream` streams like `/v1/agents/stream`.
//! `/v1/agents/workspaces/{id}/services/{name}/…` proxies the dev server an agent
//! started: owner devices only, with a looser path check for the service's own part,
//! a few more headers both ways (conditional and range requests, `Location`, `ETag`),
//! never cookies, and `Content-Security-Policy: sandbox …` on the response, so the app
//! runs with an opaque origin and can't use the owner's session.
//!
//! Streams: at most [`STREAMS_PER_CALLER`] open `/v1/agents/stream`s per caller and
//! [`STREAMS_TOTAL`] in all; each ends on shutdown, when Lucina goes quiet for
//! [`STREAM_IDLE`] (the daemon sends a keep-alive every 15 s), or when it ends. The
//! client reconnects with `Last-Event-ID` (header, or `?lastEventId=` for a new
//! EventSource), which the daemon replays from.

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::StreamExt;

use crate::auth::{Identity, Via, CSRF_HEADER};
use crate::devices::TOKEN_PREFIX;
use crate::error::ApiError;
use crate::service::{ServiceToken, SERVICE_PREFIX};
use crate::timeline::StreamSlots;
use crate::{AppState, DeviceId};

/// The origin header the daemon reads.
pub const ORIGIN_HEADER: &str = "x-lucina-origin";
/// The MCP names its client here (`mcp:hermes`); see the module docs.
pub const CALLER_HEADER: &str = "x-secretary-caller";
/// With the MCP's agents token: `<tokenId>:<name>` (phase 9's contract).
pub const MCP_CALLER_HEADER: &str = "x-lucina-mcp-caller";
/// The MCP's delegation depth, passed on to the daemon (`0` or `1`).
pub const DEPTH_HEADER: &str = "x-lucina-delegation-depth";
/// The broker's origin.
pub const BROKER: &str = "service:infisical-broker";
/// Event posts from a busy Mac session can be larger than the server's default limit.
pub const BODY_LIMIT: usize = 1024 * 1024;
pub const STREAMS_PER_CALLER: usize = 4;
pub const STREAMS_TOTAL: usize = 40;
pub const STREAM_IDLE: Duration = Duration::from_secs(90);
/// `wait` and permission long-polls.
const LONG_POLL: Duration = Duration::from_secs(15 * 60);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// The daemon's top-level routes.
const TOP: [&str; 13] = ["permissions", "workspaces", "projects", "clone", "providers", "stream", "commands", "files", "health", "schedules", "usage", "terminals", "catalog"];
const PASS: [HeaderName; 3] = [header::CONTENT_TYPE, header::ACCEPT, HeaderName::from_static("last-event-id")];
/// Also passed on to a workspace service (the dev server an agent started).
const PASS_SERVICE: [&str; 5] = ["accept-language", "if-none-match", "if-modified-since", "range", "x-requested-with"];
/// Response headers passed back besides `Content-Type` (a service's, mostly).
const PASS_BACK: [HeaderName; 7] = [
    header::CONTENT_ENCODING,
    header::LOCATION,
    header::ETAG,
    header::LAST_MODIFIED,
    header::CONTENT_DISPOSITION,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
];
/// A proxied service runs with an opaque origin, so its scripts can't use the owner's
/// Secretary session (same value as the daemon sets).
pub const SERVICE_CSP: &str = "sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads";

/// Credentials for the callers that are not devices.
#[derive(Clone, Debug, Default)]
pub struct AgentsConfig {
    /// `SECRETARY_MCP_AGENTS_TOKEN_FILE`: the MCP's own token for these routes.
    pub mcp_token: Option<ServiceToken>,
    /// `SECRETARY_AGENTS_BROKER_TOKEN_FILE`: the Infisical broker's token.
    pub broker_token: Option<ServiceToken>,
    /// `SECRETARY_MCP_DEVICE_IDS`: devices whose token the MCP holds (comma-separated).
    pub mcp_devices: Vec<String>,
}

/// The way to the daemon.
pub struct Agents {
    http: reqwest::Client,
    base: String,
    auth: String,
    cfg: AgentsConfig,
    streams: StreamSlots,
}

impl Agents {
    pub fn new(lucina_url: &str, lucina_token: &str, cfg: AgentsConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("secretary-server/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(STREAM_IDLE)
            .no_gzip()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base: lucina_url.trim_end_matches('/').to_owned(),
            auth: format!("Bearer {lucina_token}"),
            cfg,
            streams: StreamSlots::new(STREAMS_PER_CALLER, STREAMS_TOTAL),
        })
    }

    /// `GET <daemon path>` as `origin`, for this server's own reads (the system
    /// overview's catalog). The error is one short line, never the URL or a body.
    pub async fn get_json(&self, daemon_path: &str, origin: &str, timeout: Duration) -> Result<serde_json::Value, String> {
        let url = format!("{}/api/secretary/agentd{daemon_path}", self.base);
        let up = self
            .http
            .get(&url)
            .header(header::AUTHORIZATION, &self.auth)
            .header(ORIGIN_HEADER, origin)
            .header(header::ACCEPT, "application/json")
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| if e.is_timeout() { "Lucina did not answer in time".to_owned() } else { "Lucina unreachable".to_owned() })?;
        let status = up.status();
        if !status.is_success() {
            let code = up
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["error"]["code"].as_str().or(v["code"].as_str()).or(v["error"].as_str()).map(str::to_owned));
            return Err(match code {
                Some(c) => format!("agentd answered {}: {c}", status.as_u16()),
                None => format!("agentd answered {}", status.as_u16()),
            });
        }
        up.json().await.map_err(|e| if e.is_timeout() { "Lucina did not answer in time".to_owned() } else { "agentd sent no JSON".to_owned() })
    }
}

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Device(String),
    Mcp(String),
    Service(&'static str),
}

impl Caller {
    pub fn origin(&self) -> String {
        match self {
            Caller::Device(id) => format!("device:{id}"),
            Caller::Mcp(name) => format!("mcp:{name}"),
            Caller::Service(name) => format!("service:{name}"),
        }
    }

    /// The rate-limit and stream-slot key, and the access log's device.
    fn key(&self) -> String {
        match self {
            Caller::Device(id) => id.clone(),
            Caller::Mcp(name) => format!("mcp:{name}"),
            Caller::Service(name) => format!("svc:{name}"),
        }
    }
}

/// The token id in `X-Lucina-Mcp-Caller: <tokenId>:<name>` (`[A-Za-z0-9_:-]`, up to 80).
pub fn mcp_token_id(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(MCP_CALLER_HEADER)?.to_str().ok()?.trim();
    let ok = (1..=80).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'-'));
    let id = v.split(':').next().filter(|id| ok && !id.is_empty())?;
    Some(id.to_owned())
}

/// `X-Secretary-Caller: mcp:<name>` (`[a-z0-9:-]`, up to 40 characters).
pub fn mcp_name(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(CALLER_HEADER)?.to_str().ok()?.trim();
    let name = v.strip_prefix("mcp:")?;
    let ok = (1..=40).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b':' || b == b'-');
    ok.then(|| name.to_owned())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
}

fn forbidden(message: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", message)
}

/// The caller from the request's credentials (module docs).
pub fn authenticate(st: &AppState, cfg: &AgentsConfig, method: &Method, headers: &HeaderMap) -> Result<Caller, ApiError> {
    if let Some(token) = bearer(headers).filter(|t| t.starts_with(SERVICE_PREFIX)) {
        if cfg.broker_token.as_ref().is_some_and(|t| t.matches(token)) {
            return Ok(Caller::Service("infisical-broker"));
        }
        if cfg.mcp_token.as_ref().is_some_and(|t| t.matches(token)) {
            return Ok(Caller::Mcp(mcp_token_id(headers).unwrap_or_else(|| "unknown".into())));
        }
        if st.service_token.as_ref().is_some_and(|t| t.matches(token)) {
            return Err(forbidden("Lucina's service token is not for the agents API"));
        }
        return Err(ApiError::unauthorized("unknown service token"));
    }
    let ident: Identity = crate::auth::identify(st, headers)?.map_err(ApiError::unauthorized)?;
    if ident.via == Via::Bearer && !bearer(headers).is_some_and(|t| t.starts_with(TOKEN_PREFIX)) {
        return Err(ApiError::unauthorized("missing or malformed device token"));
    }
    let writes = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if ident.via == Via::Cookie && writes && headers.get(CSRF_HEADER).is_none_or(|v| v.as_bytes() != b"1") {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "csrf",
            "cookie-authenticated requests that change state need X-Secretary-CSRF: 1",
        ));
    }
    // A device token the MCP holds is the MCP, whatever it says; any device may say it is.
    if let Some(name) = mcp_name(headers) {
        return Ok(Caller::Mcp(name));
    }
    if cfg.mcp_devices.iter().any(|d| d == &ident.device_id) {
        return Ok(Caller::Mcp("device".into()));
    }
    Ok(Caller::Device(ident.device_id))
}

/// A workspace service path, `/workspaces/<id>/services/<name>[/<path>]`: the
/// service's own part may end in `/` and use any URL path character (RFC 3986
/// `pchar`), never a `.` or `..` segment.
pub fn service_path(rest: &str) -> bool {
    let mut segs = rest.strip_prefix('/').unwrap_or("").splitn(5, '/');
    let (Some("workspaces"), Some(id), Some("services"), Some(name)) = (segs.next(), segs.next(), segs.next(), segs.next()) else { return false };
    let strict = |s: &str| !s.is_empty() && s.len() <= 128 && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    if !strict(id) || !strict(name) {
        return false;
    }
    let tail = segs.next().unwrap_or("");
    rest.len() <= 2048
        && tail.split('/').all(|seg| seg != "." && seg != ".." && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%!$&'()*+,;=:@".contains(&b)))
}

/// `/terminals/<id>/stream`: a terminal's output (SSE).
fn terminal_stream(daemon_path: &str) -> bool {
    matches!(daemon_path.trim_start_matches('/').split('/').collect::<Vec<_>>().as_slice(), ["terminals", _, "stream"])
}

/// The daemon path for a `/v1/agents…` path (module docs), or `None` for a bad one.
pub fn daemon_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/agents")?;
    if rest.is_empty() || rest == "/" {
        return Some("/agents".into());
    }
    if service_path(rest) {
        return Some(rest.to_owned());
    }
    let segs = rest.strip_prefix('/')?;
    let ok = rest.len() <= 512
        && segs.split('/').all(|seg| {
            !seg.is_empty() && seg != "." && seg != ".." && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%:@".contains(&b))
        });
    if !ok {
        return None;
    }
    let first = segs.split('/').next().unwrap_or("");
    Some(if TOP.contains(&first) { rest.to_owned() } else { format!("/agents{rest}") })
}

/// What a caller may do, beyond what the daemon checks itself.
pub fn allowed(caller: &Caller, method: &Method, daemon_path: &str) -> Result<(), ApiError> {
    let segs: Vec<&str> = daemon_path.trim_start_matches('/').split('/').collect();
    match caller {
        Caller::Service(_) => {
            let ok = matches!((method, segs.as_slice()), (&Method::POST, ["permissions"]) | (&Method::GET, ["permissions", _]));
            if ok {
                Ok(())
            } else {
                Err(forbidden("this token may only ask the owner (POST /v1/agents/permissions) and wait for the answer"))
            }
        }
        Caller::Mcp(_) => {
            let answer = *method == Method::POST
                && matches!(segs.as_slice(), ["permissions", _] | ["agents", _, "permissions", _]);
            if answer {
                Err(forbidden("only the owner's devices answer permission requests"))
            } else if service_path(daemon_path) {
                Err(forbidden("only the owner's devices open workspace services"))
            } else {
                Ok(())
            }
        }
        Caller::Device(_) => Ok(()),
    }
}

fn timeout_for(method: &Method, daemon_path: &str, query: Option<&str>) -> Option<Duration> {
    if daemon_path == "/stream" || terminal_stream(daemon_path) {
        return None;
    }
    let long = (*method == Method::POST && daemon_path.ends_with("/wait"))
        || (*method == Method::GET && daemon_path.starts_with("/permissions/") && query.is_some_and(|q| q.contains("waitMs=")));
    Some(if long { LONG_POLL } else { DEFAULT_TIMEOUT })
}

fn upstream_error(e: &reqwest::Error) -> ApiError {
    tracing::warn!(timeout = e.is_timeout(), "agents: Lucina unreachable");
    if e.is_timeout() {
        ApiError::new(StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", "Lucina did not answer in time")
    } else {
        ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "Lucina is unreachable")
    }
}

async fn handle(State(st): State<AppState>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Response, ApiError> {
    let Some(agents) = st.agents.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "lucina_not_configured",
            "SECRETARY_LUCINA_TOKEN_FILE is not set on the server",
        ));
    };
    let caller = authenticate(&st, &agents.cfg, &method, &headers)?;
    let path = daemon_path(uri.path()).ok_or_else(ApiError::not_found)?;
    allowed(&caller, &method, &path)?;
    // A Mac posting its session's events uses the device-events bucket.
    let limiter = if path.ends_with("/events") { &st.device_limiter } else { &st.limiter };
    limiter.check(&caller.key()).map_err(ApiError::rate_limited)?;

    let stream = path == "/stream" || terminal_stream(&path);
    let service = service_path(&path);
    let slot = match stream {
        true => Some(agents.streams.try_acquire(&caller.key()).ok_or_else(|| {
            let mut e = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too_many_streams", "too many open agents streams; close one first");
            e.retry_after = Some(5);
            e
        })?),
        false => None,
    };
    let mut url = format!("{}/api/secretary/agentd{path}", agents.base);
    // `?lastEventId=` (a new EventSource can't set the header) passes as it is.
    let query = uri.query().filter(|q| !q.is_empty()).map(str::to_owned);
    if let Some(q) = &query {
        url.push('?');
        url.push_str(q);
    }
    let mut req = agents
        .http
        .request(method.clone(), &url)
        .header(header::AUTHORIZATION, &agents.auth)
        .header(ORIGIN_HEADER, caller.origin());
    for name in &PASS {
        if let Some(v) = headers.get(name) {
            req = req.header(name, v);
        }
    }
    if service {
        for name in PASS_SERVICE {
            if let Some(v) = headers.get(name) {
                req = req.header(name, v);
            }
        }
    }
    if let Some(d) = headers.get(DEPTH_HEADER).filter(|v| matches!(v.as_bytes(), b"0" | b"1")) {
        req = req.header(DEPTH_HEADER, d);
    }
    if let Some(t) = timeout_for(&method, &path, query.as_deref()) {
        req = req.timeout(t);
    }
    if !body.is_empty() || matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        req = req.body(body);
    }
    let up = req.send().await.map_err(|e| upstream_error(&e))?;
    let status = up.status();
    if status == StatusCode::UNAUTHORIZED {
        // Lucina refused this server's token: not the client's fault.
        tracing::error!("agents: Lucina refused the server's token");
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "lucina_auth", "Lucina refused the server's credentials"));
    }
    let content_type = up.headers().get(header::CONTENT_TYPE).cloned();
    let back: Vec<(HeaderName, HeaderValue)> = PASS_BACK.iter().filter_map(|n| Some((n.clone(), up.headers().get(n)?.clone()))).collect();
    let mut resp = if stream && status.is_success() {
        let mut shutdown = st.shutdown.subscribe();
        let body = up
            .bytes_stream()
            .take_while(|chunk| std::future::ready(chunk.is_ok()))
            .take_until(async move {
                let _ = shutdown.wait_for(|stop| *stop).await;
            })
            .map(move |chunk| {
                let _held = &slot;
                chunk
            });
        let mut resp = Body::from_stream(body).into_response();
        resp.headers_mut().insert(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no"));
        resp
    } else {
        Body::from_stream(up.bytes_stream()).into_response()
    };
    *resp.status_mut() = status;
    if let Some(ct) = content_type {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for (n, v) in back {
        resp.headers_mut().insert(n, v);
    }
    if service {
        resp.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(SERVICE_CSP));
    }
    resp.extensions_mut().insert(DeviceId(caller.key()));
    Ok(resp)
}

/// The routes, with their own authentication (module docs).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/agents", any(handle))
        .route("/v1/agents/{*rest}", any(handle))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_contract() {
        for (public, daemon) in [
            ("/v1/agents", "/agents"),
            ("/v1/agents/", "/agents"),
            ("/v1/agents/items", "/agents/items"),
            ("/v1/agents/0b6e/send", "/agents/0b6e/send"),
            ("/v1/agents/0b6e/permissions/r1", "/agents/0b6e/permissions/r1"),
            ("/v1/agents/0b6e/events", "/agents/0b6e/events"),
            ("/v1/agents/0b6e/commands/cmd_1/ack", "/agents/0b6e/commands/cmd_1/ack"),
            ("/v1/agents/permissions", "/permissions"),
            ("/v1/agents/permissions/ext_1", "/permissions/ext_1"),
            ("/v1/agents/workspaces/wks_1/diff", "/workspaces/wks_1/diff"),
            ("/v1/agents/projects", "/projects"),
            ("/v1/agents/clone", "/clone"),
            ("/v1/agents/providers", "/providers"),
            ("/v1/agents/stream", "/stream"),
            ("/v1/agents/commands", "/commands"),
            ("/v1/agents/files/download", "/files/download"),
            ("/v1/agents/terminals", "/terminals"),
            ("/v1/agents/terminals/term_1/stream", "/terminals/term_1/stream"),
            ("/v1/agents/workspaces/wks_1/scripts/web/start", "/workspaces/wks_1/scripts/web/start"),
            ("/v1/agents/workspaces/wks_1/services/web/", "/workspaces/wks_1/services/web/"),
            ("/v1/agents/workspaces/wks_1/services/web/assets/i-a+b=.js", "/workspaces/wks_1/services/web/assets/i-a+b=.js"),
            ("/v1/agents/schedules", "/schedules"),
            ("/v1/agents/schedules/ab12cd34/run-once", "/schedules/ab12cd34/run-once"),
            ("/v1/agents/usage", "/usage"),
            ("/v1/agents/search", "/agents/search"),
            ("/v1/agents/0b6e/prompts", "/agents/0b6e/prompts"),
            ("/v1/agents/0b6e/detach", "/agents/0b6e/detach"),
            ("/v1/agents/catalog", "/catalog"),
            ("/v1/agents/catalog/impact/pi-home%2Cpokoin-db-writer", "/catalog/impact/pi-home%2Cpokoin-db-writer"),
            ("/v1/agents/catalog/components/codevira@cardvault", "/catalog/components/codevira@cardvault"),
        ] {
            assert_eq!(daemon_path(public).as_deref(), Some(daemon), "{public}");
        }
        for bad in [
            "/v1/agentsx",
            "/v1/agents//x",
            "/v1/agents/../x",
            "/v1/agents/a b",
            "/v1/agents/./x",
            "/v1/agents/workspaces/wks_1/services/web/../../agents",
            "/v1/agents/workspaces/wks_1/services/../x/",
            "/v1/agents/workspaces/wks_1/services/web/a b",
        ] {
            assert_eq!(daemon_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn services_only_ask_and_mcp_never_answers() {
        let svc = Caller::Service("infisical-broker");
        assert!(allowed(&svc, &Method::POST, "/permissions").is_ok());
        assert!(allowed(&svc, &Method::GET, "/permissions/ext_1").is_ok());
        for (m, p) in [
            (Method::GET, "/permissions"),
            (Method::POST, "/permissions/ext_1"),
            (Method::GET, "/agents"),
            (Method::POST, "/agents"),
            (Method::GET, "/stream"),
        ] {
            assert!(allowed(&svc, &m, p).is_err(), "{m} {p}");
        }
        let mcp = Caller::Mcp("hermes".into());
        assert!(allowed(&mcp, &Method::POST, "/permissions/ext_1").is_err());
        assert!(allowed(&mcp, &Method::POST, "/agents/a/permissions/r").is_err());
        assert!(allowed(&mcp, &Method::GET, "/permissions").is_ok());
        assert!(allowed(&mcp, &Method::POST, "/agents/a/send").is_ok());
        assert!(allowed(&Caller::Device("d".into()), &Method::POST, "/permissions/ext_1").is_ok());
        assert!(allowed(&mcp, &Method::GET, "/workspaces/w/services/web/").is_err(), "services are the owner's");
        assert!(allowed(&mcp, &Method::GET, "/terminals/t/capture").is_ok());
        assert!(allowed(&Caller::Device("d".into()), &Method::GET, "/workspaces/w/services/web/").is_ok());
    }

    #[test]
    fn mcp_names() {
        let mut h = HeaderMap::new();
        assert_eq!(mcp_name(&h), None);
        h.insert(CALLER_HEADER, HeaderValue::from_static("mcp:hermes"));
        assert_eq!(mcp_name(&h).as_deref(), Some("hermes"));
        h.insert(CALLER_HEADER, HeaderValue::from_static("mcp:Bad Name"));
        assert_eq!(mcp_name(&h), None);
        h.insert(CALLER_HEADER, HeaderValue::from_static("app"));
        assert_eq!(mcp_name(&h), None);
        assert_eq!(Caller::Mcp("tok:1".into()).origin(), "mcp:tok:1");
        let mut h = HeaderMap::new();
        assert_eq!(mcp_token_id(&h), None);
        h.insert(MCP_CALLER_HEADER, HeaderValue::from_static("tok_AbC9:hermes"));
        assert_eq!(mcp_token_id(&h).as_deref(), Some("tok_AbC9"));
        h.insert(MCP_CALLER_HEADER, HeaderValue::from_static("tok 1:hermes"));
        assert_eq!(mcp_token_id(&h), None);
        h.insert(MCP_CALLER_HEADER, HeaderValue::from_static(":hermes"));
        assert_eq!(mcp_token_id(&h), None);
        assert_eq!(Caller::Device("dev_1".into()).origin(), "device:dev_1");
    }

    #[test]
    fn long_polls_get_long_timeouts() {
        assert_eq!(timeout_for(&Method::GET, "/stream", None), None);
        assert_eq!(timeout_for(&Method::GET, "/terminals/t/stream", None), None);
        assert_eq!(timeout_for(&Method::POST, "/agents/a/wait", None), Some(LONG_POLL));
        assert_eq!(timeout_for(&Method::GET, "/permissions/ext_1", Some("waitMs=60000")), Some(LONG_POLL));
        assert_eq!(timeout_for(&Method::GET, "/permissions/ext_1", None), Some(DEFAULT_TIMEOUT));
        assert_eq!(timeout_for(&Method::GET, "/agents", None), Some(DEFAULT_TIMEOUT));
    }
}
