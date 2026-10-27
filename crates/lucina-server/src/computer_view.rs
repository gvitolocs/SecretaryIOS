//! The live-view proxy: `/api/secretary/computer/view/{viewId}/{*rest}` →
//! the sandbox's own cua-spacesd viewer (`crate::computer` mints the view).
//!
//! secretary-server forwards the apps' viewer traffic here (bearer-authorized
//! like every `/api/secretary` call); the view id is an unguessable capability
//! bound to one sandbox, one mode and, for control views, the driver epoch at
//! minting. Only what Cua's web viewer needs passes:
//!
//! - `GET viewer/…`: the viewer's own seven static files;
//! - `POST`/`OPTIONS cua.env.v1.{Stream,Presence}Service/*` and
//!   `SystemService/{GetCapabilities,Health}` (Connect RPCs, authorized by the
//!   viewer ticket the sandbox checks itself);
//! - `GET media` with `Upgrade: websocket`: the H.264/JPEG and input socket,
//!   spliced byte for byte;
//! - on **control** views only, `ComputerService/{GetClipboard,SetClipboard}`:
//!   the viewer's clipboard sync (it polls the desktop's clipboard every second
//!   and sets it from a paste before sending Ctrl+V). Text only: both messages are
//!   rewritten to drop images and file paths, text is capped at
//!   [`CLIP_TEXT_LIMIT`], and each change is audited as a byte count, never the
//!   text.
//!
//! Files, ticket minting and every other RPC are refused. A control view dies
//! when the owner gives the desktop back or the lease lapses: requests answer
//! 410 and its socket is closed within two seconds. Socket traffic from the
//! owner's browser is input, logged as byte counts every five seconds. URLs
//! carry the ticket (`/media?ticket=`), so query strings are never logged.
//!
//! ## Why connecting used to be slow, and what changed
//!
//! A view's `index.html` carries the ticket in its fragment, so it can never be
//! cached — but the 258 KB `viewer.js` next to it was served from the same
//! per-view URL, so every view (and every reload, take-over and ticket renewal)
//! re-downloaded it through two proxies. Three things fixed that:
//!
//! - **Assets are versioned and immutable.** `GET …/viewer-assets/{version}/{file}`
//!   serves the sandbox's viewer files from an in-memory bundle keyed by the
//!   hash of their bytes, with `Cache-Control: public, max-age=31536000,
//!   immutable`. `index.html` is rewritten to reference them relatively
//!   (`../../../viewer-assets/<version>/…`), which lands on this crate's route
//!   and, one proxy up, on secretary-server's own copy of it. The ticket is
//!   still required for `index.html` and for the WebSocket, so nothing about a
//!   view is reachable without one — only Cua's public static files are.
//! - **Tickets are reused.** Minting one spawns the Cua CLI (~0.3 s); one
//!   ticket serves every view of a sandbox and mode until shortly before it
//!   expires (`viewer::mint_cached`).
//! - **Take-over and renewal do not reload the viewer.** `POST …/view/{id}/ticket`
//!   re-issues a ticket for the same view id (optionally switching watch↔control
//!   when the lease allows it) and the page hands it to the running viewer with
//!   `postMessage`. `viewer/lucina-shim.js`, injected into `index.html`, applies
//!   it to `window.__cuaViewer` and re-attaches the media session when the mode
//!   changed; see that file for why the session has to be rebuilt.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::TokioIo;
use lucina_computer_use::viewer::{ViewMode, ViewerConfig};
use lucina_computer_use::{control, SandboxId};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::api::ApiError;
use crate::AppState;

/// Views kept at once (each app tab holds one).
const MAX_VIEWS: usize = 32;
/// Largest RPC body the viewer sends.
const RPC_BODY_LIMIT: usize = 256 * 1024;
/// Clipboard text passed either way (secretary-server caps request bodies at
/// 256 KiB too). Longer text is not synced.
const CLIP_TEXT_LIMIT: usize = 256 * 1024;
/// Largest `GetClipboard` answer read before its image is dropped (Cua sends
/// PNGs up to 16 MiB).
const CLIP_RESPONSE_LIMIT: usize = 20 * 1024 * 1024;
const INPUT_LOG_EVERY: Duration = Duration::from_secs(5);
const WATCHDOG_EVERY: Duration = Duration::from_secs(2);

const ASSETS: [&str; 7] = [
    "viewer",
    "viewer/",
    "viewer/index.html",
    "viewer/index.css",
    "viewer/viewer.js",
    "viewer/audioWorklet.js",
    "viewer/captureWorklet.js",
];

/// The sandbox's viewer files, fetched once and served by bundle version.
const VIEWER_FILES: [&str; 4] = [
    "viewer.js",
    "index.css",
    "audioWorklet.js",
    "captureWorklet.js",
];

/// Every file `…/viewer-assets/{version}/{file}` may serve (plus our shim).
const ASSET_FILES: [&str; 5] = [
    "viewer.js",
    "index.css",
    "audioWorklet.js",
    "captureWorklet.js",
    "lucina-shim.js",
];

/// The first-party adapter injected into `index.html` (see that file).
const SHIM: &str = include_str!("../viewer/lucina-shim.js");

/// How `index.html` references the assets: up from `…/computer/view/<id>/viewer/`
/// to `…/computer/`, so the same rewrite works under this crate's
/// `/api/secretary/computer/` and under secretary-server's `/v1/computer/`.
const ASSETS_UP: &str = "../../../viewer-assets";

/// Request headers passed to the sandbox.
const PASS_REQUEST: [&str; 9] = [
    "content-type",
    "connect-protocol-version",
    "connect-timeout-ms",
    "x-cua-env-authorization",
    "accept",
    "access-control-request-method",
    "access-control-request-headers",
    "x-grpc-web",
    "x-user-agent",
];

/// Response headers passed back (plus `grpc-*`/`connect-*`/`access-control-*`).
const PASS_RESPONSE: [&str; 5] = [
    "content-type",
    "content-encoding",
    "cache-control",
    "etag",
    "last-modified",
];

/// The viewer runs in an opaque origin: even when opened at top level, Cua's
/// page never gets the app's origin (cookies, storage).
pub const VIEW_CSP: &str = "sandbox allow-scripts allow-pointer-lock; frame-ancestors 'self'";

#[derive(Debug, Clone)]
struct View {
    /// The capability in the URL; `reissue` finds the entry by it.
    id: String,
    sandbox: SandboxId,
    mode: ViewMode,
    device: String,
    epoch: u64,
    machine: String,
    expires: Instant,
}

fn views() -> &'static Mutex<HashMap<String, View>> {
    static V: OnceLock<Mutex<HashMap<String, View>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a minted view and returns its id (64 hex chars, 244 random bits).
pub fn open(sandbox: SandboxId, mode: ViewMode, device: &str, epoch: u64, machine: &str) -> String {
    let id = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    map.retain(|_, v| v.expires > now);
    if map.len() >= MAX_VIEWS {
        // Oldest first: a forgotten tab never blocks a new view.
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, v)| v.expires)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(
        id.clone(),
        View {
            id: id.clone(),
            sandbox,
            mode,
            device: device.to_owned(),
            epoch,
            machine: machine.to_owned(),
            // A little longer than the ticket: the sandbox refuses it first.
            expires: now + lucina_computer_use::viewer::VIEW_TTL + Duration::from_secs(30),
        },
    );
    id
}

/// Forgets every control view of a sandbox (the owner gave it back).
pub fn drop_control_views(sb: SandboxId) {
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, v| !(v.sandbox == sb && v.mode == ViewMode::Control));
}

/// The view, if it is still alive. A control view also needs the owner's lease
/// under the epoch it was minted in.
fn live(id: &str) -> Option<View> {
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    let v = map.get(id)?.clone();
    let alive = v.expires > Instant::now()
        && (v.mode == ViewMode::Watch
            || (control().owner_driving(v.sandbox) && control().epoch(v.sandbox) == v.epoch));
    if !alive {
        map.remove(id);
        return None;
    }
    Some(v)
}

#[derive(Debug, PartialEq, Eq)]
enum Target {
    Asset,
    Rpc,
    /// `ComputerService/{Get,Set}Clipboard`: control views only, text only.
    Clipboard(Clip),
    Socket,
    /// `POST ticket`: re-issue this view's ticket (renew, or switch mode).
    Ticket,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Clip {
    /// The desktop's clipboard, to the owner's.
    Get,
    /// The owner's clipboard (from a paste), to the desktop.
    Set,
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn classify(method: &Method, rest: &str, upgrade: bool) -> Option<Target> {
    if upgrade {
        return (method == Method::GET && rest == "media").then_some(Target::Socket);
    }
    if rest == "ticket" {
        return (method == Method::POST).then_some(Target::Ticket);
    }
    if ASSETS.contains(&rest) {
        return (method == Method::GET || method == Method::HEAD).then_some(Target::Asset);
    }
    let (svc, rpc) = rest.strip_prefix("cua.env.v1.")?.split_once('/')?;
    if !is_ident(svc) || !is_ident(rpc) || !(method == Method::POST || method == Method::OPTIONS) {
        return None;
    }
    if svc == "ComputerService" {
        return match rpc {
            "GetClipboard" => Some(Target::Clipboard(Clip::Get)),
            "SetClipboard" => Some(Target::Clipboard(Clip::Set)),
            _ => None,
        };
    }
    let allowed = matches!(svc, "StreamService" | "PresenceService")
        || (svc == "SystemService" && matches!(rpc, "GetCapabilities" | "Health"));
    allowed.then_some(Target::Rpc)
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn harden(h: &mut HeaderMap) {
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(VIEW_CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-site"),
    );
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("view proxy client")
    })
}

/// Cua's viewer files, fetched once from a sandbox and served by content hash.
#[derive(Debug)]
struct Bundle {
    /// 16 hex chars: the hash of every name and byte in `files`.
    version: String,
    files: HashMap<&'static str, Vec<u8>>,
}

impl Bundle {
    fn get(&self, file: &str) -> Option<&Vec<u8>> {
        self.files.get(file)
    }
}

fn bundle_slot() -> &'static Mutex<Option<Arc<Bundle>>> {
    static B: OnceLock<Mutex<Option<Arc<Bundle>>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(None))
}

/// FNV-1a over the file names and bytes: cache-busting, not a security boundary.
fn bundle_hash(files: &HashMap<&'static str, Vec<u8>>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    };
    for name in ASSET_FILES {
        for b in name.as_bytes() {
            eat(*b);
        }
        if let Some(bytes) = files.get(name) {
            for b in bytes {
                eat(*b);
            }
        }
    }
    format!("{h:016x}")
}

/// The cached bundle, or a fresh fetch from a sandbox's `viewer/`.
async fn bundle() -> Result<Arc<Bundle>, ApiError> {
    if let Some(b) = bundle_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Ok(b);
    }
    let built = Arc::new(load_bundle().await?);
    *bundle_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(built.clone());
    Ok(built)
}

/// Forces the next `bundle()` to fetch again (the sandbox was upgraded).
fn forget_bundle() {
    *bundle_slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

async fn load_bundle() -> Result<Bundle, ApiError> {
    let cfg = ViewerConfig::from_env();
    let machine = lucina_computer_use::viewer::machine_url(&cfg, SandboxId::Workspace)
        .or_else(|_| lucina_computer_use::viewer::machine_url(&cfg, SandboxId::Android))
        .map_err(|e| {
            tracing::warn!(error = %e, "live view: no sandbox to fetch the viewer assets from");
            ApiError::new(
                503,
                "viewer_unavailable",
                "the sandbox viewer is not reachable",
            )
        })?;
    let mut files: HashMap<&'static str, Vec<u8>> = HashMap::new();
    for name in VIEWER_FILES {
        let r = http()
            .get(format!("{machine}/viewer/{name}"))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e.without_url(), name, "live view: viewer asset fetch failed");
                ApiError::new(502, "sandbox_unreachable", "")
            })?;
        if !r.status().is_success() {
            tracing::warn!(name, status = %r.status(), "live view: viewer asset missing");
            return Err(ApiError::new(
                502,
                "sandbox_refused",
                "the sandbox has no viewer assets",
            ));
        }
        let bytes = r.bytes().await.map_err(|e| {
            tracing::warn!(error = %e.without_url(), name, "live view: viewer asset body failed");
            ApiError::new(502, "sandbox_unreachable", "")
        })?;
        files.insert(name, bytes.to_vec());
    }
    // Ours, not Cua's: served from the same immutable path.
    files.insert("lucina-shim.js", SHIM.as_bytes().to_vec());
    let version = bundle_hash(&files);
    tracing::info!(version = %version, "live view: viewer assets cached");
    Ok(Bundle { version, files })
}

/// The cached bundle's version, or `""` when no sandbox answers. The apps
/// preload `viewer-assets/<version>/viewer.js` with it while a view is minted.
pub async fn viewer_version() -> String {
    match bundle().await {
        Ok(b) => b.version.clone(),
        Err(_) => String::new(),
    }
}

/// The only names the immutable asset route will ever serve.
fn serves_asset(file: &str) -> bool {
    ASSET_FILES.contains(&file)
}

fn content_type(file: &str) -> &'static str {
    match file {
        "index.css" => "text/css; charset=utf-8",
        _ => "text/javascript; charset=utf-8",
    }
}

/// `GET /api/secretary/computer/viewer-assets/{version}/{file}`: Cua's viewer
/// files plus our shim, cacheable forever and reachable without a ticket (they
/// are public static files; the view itself still needs one).
pub async fn assets(
    State(st): State<AppState>,
    Path((version, file)): Path<(String, String)>,
    req: Request,
) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    asset(&version, &file).await
}

/// One versioned viewer file (the bearer, if any, was checked by the caller).
pub async fn asset(version: &str, file: &str) -> Response {
    if !serves_asset(file) || version.len() != 16 || !version.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return ApiError::new(404, "not_found", "").into_response();
    }
    let mut b = match bundle().await {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    if b.version != version {
        // A different sandbox build (or a restart after an upgrade): refetch once.
        forget_bundle();
        b = match bundle().await {
            Ok(b) => b,
            Err(e) => return e.into_response(),
        };
        if b.version != version {
            return ApiError::new(404, "not_found", "").into_response();
        }
    }
    let Some(bytes) = b.get(file) else {
        return ApiError::new(404, "not_found", "").into_response();
    };
    let mut resp = Response::new(Body::from(bytes.clone()));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(file)),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    // The viewer document is an opaque origin (CSP `sandbox`), and `viewer.js` is a module
    // script, which is always fetched with CORS: without this the viewer never starts.
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    harden(resp.headers_mut());
    // Public static files (CORS already says so): loadable by the classic script and
    // stylesheet tags too when the view is framed from another site (the home-network door).
    resp.headers_mut().insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    resp
}

/// `ANY /api/secretary/computer/view/{viewId}/{*rest}` (bearer-authorized by the caller).
pub async fn proxy(
    State(st): State<AppState>,
    Path((view_id, rest)): Path<(String, String)>,
    req: Request,
) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    serve(&view_id, &rest, req, true).await
}

/// The view for a caller that holds only its capability URL (the home-network door,
/// `lan.rs`): everything the viewer itself needs, never the owner's ticket re-issue.
pub async fn serve_capability(view_id: &str, rest: &str, req: Request) -> Response {
    serve(view_id, rest, req, false).await
}

async fn serve(view_id: &str, rest: &str, req: Request, owner: bool) -> Response {
    let Some(view) = live(view_id) else {
        return ApiError::new(
            410,
            "view_ended",
            "this live view has ended; open a new one",
        )
        .into_response();
    };
    let upgrade = is_upgrade(req.headers());
    let target = classify(req.method(), rest, upgrade)
        .filter(|t| owner || *t != Target::Ticket)
        // Watching never touches the clipboard; a control view lives only
        // under the owner's lease (`live`).
        .filter(|t| !matches!(t, Target::Clipboard(_)) || view.mode == ViewMode::Control);
    let Some(target) = target else {
        tracing::warn!(sandbox = view.sandbox.as_str(), method = %req.method(), path = %rest, "live view request refused");
        return ApiError::new(403, "not_allowed", "").into_response();
    };
    let mut resp = match target {
        Target::Socket => socket(view, req).await,
        Target::Ticket => ticket(view, req).await,
        Target::Asset if rest == "viewer/index.html" || rest == "viewer/" => {
            index_html(&view).await
        }
        Target::Asset | Target::Rpc => forward(&view, rest, req).await,
        Target::Clipboard(_) if req.method() == Method::OPTIONS => forward(&view, rest, req).await,
        Target::Clipboard(clip) => clipboard(&view, clip, rest, req).await,
    };
    harden(resp.headers_mut());
    resp
}

/// `index.html` with the ticket in its fragment (never cacheable), rewritten to
/// load the viewer from the immutable asset path and to run our shim first.
async fn index_html(view: &View) -> Response {
    let b = match bundle().await {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let r = match http()
        .get(format!("{}/viewer/index.html", view.machine))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(sandbox = view.sandbox.as_str(), error = %e.without_url(), "live view: index.html failed");
            return ApiError::new(502, "sandbox_unreachable", "").into_response();
        }
    };
    if !r.status().is_success() {
        return ApiError::new(502, "sandbox_refused", "").into_response();
    }
    let html = match r.text().await {
        Ok(t) => t,
        Err(_) => return ApiError::new(502, "sandbox_unreachable", "").into_response(),
    };
    let mut resp = Response::new(Body::from(rewrite_index(&html, &b.version)));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

/// Points `index.html` at the versioned assets and loads the shim before
/// `viewer.js`. `crossorigin` is dropped: the page is an opaque origin (CSP
/// `sandbox`), so those requests would have to pass CORS.
fn rewrite_index(html: &str, version: &str) -> String {
    let mut out = html.replace("./viewer.js", &format!("{ASSETS_UP}/{version}/viewer.js"));
    out = out.replace("./index.css", &format!("{ASSETS_UP}/{version}/index.css"));
    out = out.replace(" crossorigin", "");
    let shim = format!(
        "<script src=\"{ASSETS_UP}/{version}/lucina-shim.js\"></script>\n    <meta name=\"lucina-view\" content=\"{version}\" />\n    "
    );
    if let Some(at) = out.find("<script") {
        out.insert_str(at, &shim);
    } else {
        out.push_str(&shim);
    }
    out
}

/// `POST /api/secretary/computer/view/{viewId}/ticket` `{mode?}`: the same view,
/// with a fresh ticket (and, if asked, the other mode). Taking control needs the
/// owner's lease, exactly like a control view minted from scratch.
async fn ticket(view: View, req: Request) -> Response {
    let body = match axum::body::to_bytes(req.into_body(), 4096).await {
        Ok(b) => b,
        Err(_) => return ApiError::new(413, "too_large", "").into_response(),
    };
    let asked = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(v) => v,
        Err(_) => serde_json::Value::Null,
    };
    let mode = match asked.get("mode").and_then(serde_json::Value::as_str) {
        None => view.mode,
        Some(m) => match ViewMode::parse(m) {
            Some(m) => m,
            None => return ApiError::bad_request("mode must be watch or control").into_response(),
        },
    };
    if mode == ViewMode::Control && !control().owner_driving(view.sandbox) {
        return ApiError::new(409, "not_driving", "take over before driving").into_response();
    }
    let epoch = control().epoch(view.sandbox);
    let (minted, left) = match lucina_computer_use::viewer::mint_cached(
        &ViewerConfig::from_env(),
        view.sandbox,
        mode,
        lucina_computer_use::viewer::VIEW_TTL,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(sandbox = view.sandbox.as_str(), error = %e, "live view: ticket re-issue failed");
            return ApiError::new(
                503,
                "viewer_unavailable",
                "the sandbox refused a viewer ticket",
            )
            .into_response();
        }
    };
    let Some(updated) = reissue(&view, mode, epoch) else {
        return ApiError::new(
            410,
            "view_ended",
            "this live view has ended; open a new one",
        )
        .into_response();
    };
    control().note(
        view.sandbox,
        "view_ticket",
        json!({"device": view.device, "mode": mode.as_str(), "same_view": true}),
    );
    // The API's own envelope (`{ok, data}`): secretary-server forwards it like any other call.
    let mut resp = crate::api::ok(json!({
        "ticket": minted.ticket,
        "mode": mode.as_str(),
        "sandbox": view.sandbox.as_str(),
        "ttlSecs": left.as_secs(),
        "expiresAt": minted.expires_at,
        "epoch": epoch,
        "changed": updated.mode != view.mode,
    }));
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Switches a registered view in place: same id, new mode, epoch and expiry.
fn reissue(view: &View, mode: ViewMode, epoch: u64) -> Option<View> {
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    let v = map.get_mut(&view.id)?;
    v.mode = mode;
    v.epoch = epoch;
    v.expires = Instant::now() + lucina_computer_use::viewer::VIEW_TTL + Duration::from_secs(30);
    Some(v.clone())
}

async fn forward(view: &View, rest: &str, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, RPC_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return ApiError::new(413, "too_large", "").into_response(),
    };
    let Some(up) = send_upstream(view, rest, &parts, body.to_vec()).await else {
        return ApiError::new(502, "sandbox_unreachable", "").into_response();
    };
    let status = up.status();
    let headers = pass_response_headers(up.headers());
    let mut resp = Response::new(Body::from_stream(up.bytes_stream()));
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}

async fn send_upstream(
    view: &View,
    rest: &str,
    parts: &axum::http::request::Parts,
    body: Vec<u8>,
) -> Option<reqwest::Response> {
    let mut url = format!("{}/{}", view.machine, rest);
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let mut rb = http().request(parts.method.clone(), &url).body(body);
    for name in PASS_REQUEST {
        if let Some(v) = parts.headers.get(name) {
            rb = rb.header(name, v);
        }
    }
    rb.send()
        .await
        .map_err(|e| {
            tracing::warn!(sandbox = view.sandbox.as_str(), error = %e.without_url(), "live view upstream failed");
    })
        .ok()
}

fn pass_response_headers(up: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (k, v) in up {
        let n = k.as_str();
        if PASS_RESPONSE.contains(&n)
            || n.starts_with("grpc-")
            || n.starts_with("connect-")
            || n.starts_with("access-control-")
        {
            headers.append(k.clone(), v.clone());
        }
    }
    headers
}

/// The last desktop clipboard generation each sandbox's viewer saw, so a copy on
/// the desktop is audited once rather than on every one-second poll.
fn seen_generation() -> &'static Mutex<HashMap<&'static str, u64>> {
    static SEEN: OnceLock<Mutex<HashMap<&'static str, u64>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}
/// `POST ComputerService/{Get,Set}Clipboard` (gRPC-web, binary, unary): the
/// messages are rewritten to carry text only, so neither side sees images or
/// file paths, and a change is audited as a byte count. Refusals are gRPC-web
/// errors the viewer can read (a bare HTTP error fails its CORS check).
async fn clipboard(view: &View, clip: Clip, rest: &str, req: Request) -> Response {
    let binary = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.eq_ignore_ascii_case("application/grpc-web+proto")
                || v.eq_ignore_ascii_case("application/grpc-web")
        });
    if !binary {
        return grpc_error(GRPC_UNIMPLEMENTED, "the clipboard takes binary gRPC-web");
    }
    let (parts, body) = req.into_parts();
    let Ok(body) = axum::body::to_bytes(body, RPC_BODY_LIMIT).await else {
        return grpc_error(
            GRPC_RESOURCE_EXHAUSTED,
            "clipboard text over 256 KiB is not synced",
        );
    };
    let Some(message) = grpc_web::single_message(&body) else {
        return grpc_error(GRPC_INVALID_ARGUMENT, "one uncompressed message expected");
    };
    let (message, sent) = match clip {
        // `GetClipboardRequest` has no fields.
        Clip::Get => (Vec::new(), 0),
        Clip::Set => match clip_proto::set_request_text(message) {
            Some(Some(text)) if text.len() <= CLIP_TEXT_LIMIT => {
                (clip_proto::set_request(text), text.len())
            }
            Some(Some(_)) => {
                return grpc_error(
                    GRPC_RESOURCE_EXHAUSTED,
                    "clipboard text over 256 KiB is not synced",
                )
            }
            Some(None) => return grpc_error(GRPC_UNIMPLEMENTED, "only text is synced"),
            None => return grpc_error(GRPC_INVALID_ARGUMENT, "malformed SetClipboardRequest"),
        },
    };
    let Some(up) = send_upstream(view, rest, &parts, grpc_web::frame(0, &message)).await else {
        return grpc_error(GRPC_UNAVAILABLE, "the sandbox is unreachable");
    };
    let status = up.status();
    let headers = pass_response_headers(up.headers());
    // An HTTP error, or a trailers-only gRPC error: nothing to rewrite.
    if !status.is_success() || up.headers().contains_key("grpc-status") {
        let mut resp = Response::new(Body::from_stream(up.bytes_stream()));
        *resp.status_mut() = status;
        *resp.headers_mut() = headers;
        return resp;
    }
    let Some(answer) = read_capped(up, CLIP_RESPONSE_LIMIT).await else {
        return grpc_error(GRPC_UNAVAILABLE, "the clipboard answer was too large");
    };
    let Some(frames) = grpc_web::frames(&answer) else {
        return grpc_error(GRPC_UNAVAILABLE, "malformed clipboard answer");
    };
    let ok = frames
        .iter()
        .any(|(f, t)| *f & 0x80 != 0 && grpc_web::status_ok(t));
    let body = match clip {
        Clip::Set => {
            if ok {
                control().note(
                    view.sandbox,
                    "clipboard_to_desktop",
                    json!({"device": view.device, "bytes": sent}),
                );
                // The paste's own generation, so the next poll does not audit it back.
                let set = frames.iter().find(|(f, _)| *f == 0);
                if let Some(generation) =
                    set.and_then(|(_, m)| clip_proto::set_response_generation(m))
                {
                    seen_generation()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(view.sandbox.as_str(), generation);
                }
            }
            answer
        }
        Clip::Get => {
            let mut out = Vec::with_capacity(answer.len().min(CLIP_TEXT_LIMIT + 64));
            for (flags, bytes) in &frames {
                if flags & 0x80 != 0 {
                    out.extend(grpc_web::frame(*flags, bytes));
                    continue;
                }
                if flags & 1 != 0 {
                    return grpc_error(GRPC_UNAVAILABLE, "compressed clipboard answer");
                }
                let Some((text, generation)) = clip_proto::get_response(bytes) else {
                    return grpc_error(GRPC_UNAVAILABLE, "malformed GetClipboardResponse");
                };
                let text = text.filter(|t| t.len() <= CLIP_TEXT_LIMIT);
                let before = seen_generation()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(view.sandbox.as_str(), generation);
                if ok && before.is_some_and(|g| g != generation) {
                    control().note(
                        view.sandbox,
                        "clipboard_from_desktop",
                        json!({"device": view.device, "bytes": text.map_or(0, str::len)}),
                    );
                }
                out.extend(grpc_web::frame(
                    0,
                    &clip_proto::get_response_bytes(text, generation),
                ));
            }
            out
        }
    };
    let mut resp = Response::new(Body::from(body));
    *resp.headers_mut() = headers;
    resp
}

const GRPC_INVALID_ARGUMENT: u8 = 3;
const GRPC_RESOURCE_EXHAUSTED: u8 = 8;
const GRPC_UNIMPLEMENTED: u8 = 12;
const GRPC_UNAVAILABLE: u8 = 14;

/// A trailers-only gRPC-web error, readable from the viewer's opaque origin. The
/// message never says "permission" or "not allowed": the viewer turns its
/// clipboard sync off for good on those.
fn grpc_error(code: u8, message: &'static str) -> Response {
    let mut resp = Response::new(Body::empty());
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc-web+proto"),
    );
    h.insert("grpc-status", HeaderValue::from(u16::from(code)));
    h.insert("grpc-message", HeaderValue::from_static(message));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("grpc-status, grpc-message"),
    );
    resp
}

/// gRPC-web framing: `[flags][u32 big-endian length][bytes]` per frame; flag
/// `0x80` marks the trailers frame, flag `1` a compressed message.
mod grpc_web {
    pub(super) fn frames(mut b: &[u8]) -> Option<Vec<(u8, &[u8])>> {
        let mut out = Vec::new();
        while !b.is_empty() {
            let (&flags, rest) = b.split_first()?;
            let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
            let bytes = rest.get(4..4 + len)?;
            out.push((flags, bytes));
            b = &rest[4 + len..];
        }
        Some(out)
    }

    /// The one uncompressed message of a unary request.
    pub(super) fn single_message(b: &[u8]) -> Option<&[u8]> {
        match frames(b)?.as_slice() {
            [(0, m)] => Some(m),
            _ => None,
        }
    }

    pub(super) fn frame(flags: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + bytes.len());
        out.push(flags);
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
        out
    }

    /// Whether a trailers frame says `grpc-status: 0`.
    pub(super) fn status_ok(trailers: &[u8]) -> bool {
        String::from_utf8_lossy(trailers).split("\r\n").any(|l| {
            l.split_once(':').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("grpc-status") && v.trim() == "0"
            })
        })
    }
}

async fn read_capped(mut up: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = up.chunk().await.ok()? {
        if out.len() + chunk.len() > limit {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

/// Just enough protobuf for the clipboard messages of `cua.env.v1` (field
/// numbers from the viewer's embedded descriptor):
///
/// - `ClipboardContent { optional string text = 1; repeated string file_paths = 2; optional bytes image_png = 3; }`
/// - `SetClipboardRequest { ClipboardContent content = 1; }`
/// - `GetClipboardResponse { ClipboardContent content = 1; uint64 generation = 2; }`
/// - `SetClipboardResponse { uint64 generation = 1; }`
mod clip_proto {
    enum Field<'a> {
        Varint(u64),
        Len(&'a [u8]),
        Fixed,
    }

    fn varint(b: &mut &[u8]) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let (&x, rest) = b.split_first()?;
            *b = rest;
            v |= u64::from(x & 0x7f) << (7 * i);
            if x & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// Every field of a message, or `None` if it is not well-formed.
    fn fields(mut b: &[u8]) -> Option<Vec<(u64, Field<'_>)>> {
        let mut out = Vec::new();
        while !b.is_empty() {
            let key = varint(&mut b)?;
            let field = match key & 7 {
                0 => Field::Varint(varint(&mut b)?),
                1 | 5 => {
                    let n = if key & 7 == 1 { 8 } else { 4 };
                    b = b.get(n..)?;
                    Field::Fixed
                }
                2 => {
                    let n = usize::try_from(varint(&mut b)?).ok()?;
                    let v = b.get(..n)?;
                    b = &b[n..];
                    Field::Len(v)
                }
                _ => return None,
            };
            out.push((key >> 3, field));
        }
        Some(out)
    }

    /// The text of a `ClipboardContent`: `None` if malformed, `Some(None)` if it has none.
    fn content_text(b: &[u8]) -> Option<Option<&str>> {
        let mut text = None;
        for (n, f) in fields(b)? {
            if let (1, Field::Len(v)) = (n, f) {
                text = Some(std::str::from_utf8(v).ok()?);
            }
        }
        Some(text)
    }

    pub(super) fn set_request_text(b: &[u8]) -> Option<Option<&str>> {
        let mut text = None;
        for (n, f) in fields(b)? {
            if let (1, Field::Len(v)) = (n, f) {
                text = content_text(v)?;
            }
        }
        Some(text)
    }

    /// The generation a `SetClipboardResponse { uint64 generation = 1; }` reports.
    pub(super) fn set_response_generation(b: &[u8]) -> Option<u64> {
        fields(b)?.into_iter().find_map(|(n, f)| match (n, f) {
            (1, Field::Varint(g)) => Some(g),
            _ => None,
        })
    }

    /// `(text, generation)` of a `GetClipboardResponse`.
    pub(super) fn get_response(b: &[u8]) -> Option<(Option<&str>, u64)> {
        let (mut text, mut generation) = (None, 0);
        for (n, f) in fields(b)? {
            match (n, f) {
                (1, Field::Len(v)) => text = content_text(v)?,
                (2, Field::Varint(g)) => generation = g,
                _ => {}
            }
        }
        Some((text, generation))
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn put_len(out: &mut Vec<u8>, field: u64, v: &[u8]) {
        put_varint(out, field << 3 | 2);
        put_varint(out, v.len() as u64);
        out.extend_from_slice(v);
    }

    fn content(text: Option<&str>) -> Vec<u8> {
        let mut c = Vec::new();
        if let Some(t) = text {
            put_len(&mut c, 1, t.as_bytes());
        }
        c
    }

    pub(super) fn set_request(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        put_len(&mut out, 1, &content(Some(text)));
        out
    }

    pub(super) fn get_response_bytes(text: Option<&str>, generation: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_len(&mut out, 1, &content(text));
        if generation != 0 {
            put_varint(&mut out, 2 << 3);
            put_varint(&mut out, generation);
        }
        out
    }

    #[cfg(test)]
    pub(super) fn image_content(text: Option<&str>, png: &[u8], generation: u64) -> Vec<u8> {
        let mut c = content(text);
        put_len(&mut c, 2, b"/home/lucina/secret.txt");
        put_len(&mut c, 3, png);
        let mut out = Vec::new();
        put_len(&mut out, 1, &c);
        if generation != 0 {
            put_varint(&mut out, 2 << 3);
            put_varint(&mut out, generation);
        }
        out
    }
}

/// Reads an HTTP/1.1 response head; returns `(status, headers, bytes after the head)`.
async fn read_head(s: &mut TcpStream) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "closed before the response head",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let rest = buf[end + 4..].to_vec();
            let mut lines = head.split("\r\n");
            let status = lines
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                .collect();
            return Ok((status, headers, rest));
        }
        if buf.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response head too large",
            ));
        }
    }
}

fn header_line_ok(v: &str) -> bool {
    !v.bytes().any(|b| b == b'\r' || b == b'\n')
}

/// Splices the viewer's `/media` WebSocket to the sandbox.
async fn socket(view: View, mut req: Request) -> Response {
    let Some(addr) = view.machine.strip_prefix("http://") else {
        return ApiError::new(502, "sandbox_unreachable", "").into_response();
    };
    let path_q = match req.uri().query() {
        Some(q) => format!("/media?{q}"),
        None => "/media".to_owned(),
    };
    let mut head = format!(
        "GET {path_q} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
    );
    for name in [
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
        "x-cua-env-authorization",
    ] {
        if let Some(v) = req.headers().get(name).and_then(|v| v.to_str().ok()) {
            if !header_line_ok(v) || !header_line_ok(&path_q) {
                return ApiError::bad_request("bad header").into_response();
            }
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    let mut upstream =
        match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            _ => return ApiError::new(502, "sandbox_unreachable", "").into_response(),
        };
    let _ = upstream.set_nodelay(true);
    if upstream.write_all(head.as_bytes()).await.is_err() {
        return ApiError::new(502, "sandbox_unreachable", "").into_response();
    }
    let (status, up_headers, leftover) =
        match tokio::time::timeout(Duration::from_secs(10), read_head(&mut upstream)).await {
            Ok(Ok(h)) => h,
            _ => return ApiError::new(502, "sandbox_unreachable", "").into_response(),
        };
    if status != 101 {
        tracing::warn!(
            sandbox = view.sandbox.as_str(),
            status,
            "live view socket refused by the sandbox"
        );
        return ApiError::new(502, "sandbox_refused", format!("sandbox answered {status}"))
            .into_response();
    }
    let on_upgrade = hyper::upgrade::on(&mut req);
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    for (k, v) in &up_headers {
        if matches!(
            k.as_str(),
            "upgrade"
                | "connection"
                | "sec-websocket-accept"
                | "sec-websocket-protocol"
                | "sec-websocket-extensions"
        ) {
            if let (Ok(n), Ok(v)) = (HeaderName::try_from(k.as_str()), HeaderValue::from_str(v)) {
                resp.headers_mut().insert(n, v);
            }
        }
    }
    control().note(
        view.sandbox,
        "view_socket_open",
        json!({"device": view.device, "mode": view.mode.as_str()}),
    );
    tokio::spawn(async move {
        let client = match on_upgrade.await {
            Ok(u) => TokioIo::new(u),
            Err(e) => {
                tracing::warn!(error = %e, "live view upgrade failed");
                return;
            }
        };
        splice(view, client, upstream, leftover).await;
    });
    resp
}

async fn splice<C>(view: View, client: C, upstream: TcpStream, leftover: Vec<u8>)
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut ur, mut uw) = upstream.into_split();
    let input = Arc::new(AtomicU64::new(0));
    let up_count = input.clone();
    let up = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match cr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            up_count.fetch_add(n as u64, Ordering::Relaxed);
            if uw.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };
    let down = async move {
        if !leftover.is_empty() && cw.write_all(&leftover).await.is_err() {
            return;
        }
        let _ = tokio::io::copy(&mut ur, &mut cw).await;
    };
    let watch_view = view.clone();
    let watchdog = async move {
        let mut tick = tokio::time::interval(WATCHDOG_EVERY);
        let mut last_log = Instant::now();
        let mut logged = 0u64;
        loop {
            tick.tick().await;
            if watch_view.expires <= Instant::now() {
                return "expired";
            }
            if watch_view.mode == ViewMode::Control {
                if !control().owner_driving(watch_view.sandbox)
                    || control().epoch(watch_view.sandbox) != watch_view.epoch
                {
                    return "control ended";
                }
                if last_log.elapsed() >= INPUT_LOG_EVERY {
                    let total = input.load(Ordering::Relaxed);
                    if total > logged {
                        let batch = json!({"via": "live", "bytes": total - logged});
                        if control()
                            .owner_input(watch_view.sandbox, &watch_view.device, batch)
                            .is_err()
                        {
                            return "control ended";
                        }
                        logged = total;
                    }
                    last_log = Instant::now();
                }
            }
        }
    };
    let why = tokio::select! {
        _ = up => "viewer closed",
        _ = down => "sandbox closed",
        why = watchdog => why,
    };
    control().note(
        view.sandbox,
        "view_socket_closed",
        json!({"device": view.device, "mode": view.mode.as_str(), "why": why}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that take the Android lease (`control()` is process-wide).
    static ANDROID_LEASE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn only_the_viewer_surface_is_reachable() {
        let get = Method::GET;
        let post = Method::POST;
        assert_eq!(classify(&get, "viewer/", false), Some(Target::Asset));
        assert_eq!(
            classify(&get, "viewer/viewer.js", false),
            Some(Target::Asset)
        );
        assert_eq!(classify(&post, "viewer/viewer.js", false), None);
        assert_eq!(
            classify(&post, "cua.env.v1.StreamService/OpenMedia", false),
            Some(Target::Rpc)
        );
        assert_eq!(
            classify(&Method::OPTIONS, "cua.env.v1.PresenceService/Join", false),
            Some(Target::Rpc)
        );
        assert_eq!(
            classify(&post, "cua.env.v1.SystemService/GetCapabilities", false),
            Some(Target::Rpc)
        );
        assert_eq!(classify(&get, "media", true), Some(Target::Socket));
        // The clipboard sync (`serve` keeps it to control views).
        assert_eq!(
            classify(&post, "cua.env.v1.ComputerService/GetClipboard", false),
            Some(Target::Clipboard(Clip::Get))
        );
        assert_eq!(
            classify(
                &Method::OPTIONS,
                "cua.env.v1.ComputerService/SetClipboard",
                false
            ),
            Some(Target::Clipboard(Clip::Set))
        );
        // The ticket re-issue is POST-only, and never a file or an RPC.
        assert_eq!(classify(&post, "ticket", false), Some(Target::Ticket));
        for (m, p, up) in [
            (&post, "cua.env.v1.SystemService/CreateViewerTicket", false),
            (&post, "cua.env.v1.FilesService/Read", false),
            (&post, "cua.env.v1.ComputerService/Screenshot", false),
            (&post, "cua.env.v1.ComputerService/RunCommand", false),
            (&get, "cua.env.v1.ComputerService/GetClipboard", false),
            (&get, "cua.env.v1.StreamService/OpenMedia", false),
            (&post, "cua.env.v1.Stream.Service/X", false),
            (&get, "viewer/../files", false),
            (&get, "viewer/unknown.js", false),
            (&get, "files", false),
            (&get, "media", false),
            (&get, "files", true),
            (&post, "media", true),
            (&get, "", false),
            (&get, "ticket", false),
            (&get, "viewer-assets/00/viewer.js", false),
        ] {
            assert_eq!(classify(m, p, up), None, "{m} {p} {up}");
        }
    }

    #[test]
    fn the_asset_route_serves_only_the_viewer_files() {
        for name in ASSET_FILES {
            assert!(crate::computer_view::serves_asset(name), "{name}");
        }
        for name in [
            "index.html",
            "lucina-shim.js.bak",
            "../secrets",
            "files",
            "viewer",
            "",
        ] {
            assert!(!crate::computer_view::serves_asset(name), "{name}");
        }
        assert!(
            SHIM.contains("lucina:keyboard"),
            "the shim is the one we wrote"
        );
        assert!(SHIM.contains("lucina:ticket"));
    }

    #[test]
    fn index_html_points_at_the_versioned_assets_and_runs_the_shim_first() {
        let html = rewrite_index(
            r#"<html><head><script type="module" crossorigin src="./viewer.js"></script>
    <link rel="stylesheet" crossorigin href="./index.css"></head><body></body></html>"#,
            "0123456789abcdef",
        );
        assert!(
            !html.contains("./viewer.js"),
            "the per-view URL is gone: {html}"
        );
        assert!(!html.contains("./index.css"));
        assert!(
            !html.contains("crossorigin"),
            "an opaque origin cannot do CORS"
        );
        assert!(html.contains("../../../viewer-assets/0123456789abcdef/viewer.js"));
        assert!(html.contains("../../../viewer-assets/0123456789abcdef/index.css"));
        let shim = html.find("lucina-shim.js").expect("the shim is injected");
        let viewer = html
            .find("viewer-assets/0123456789abcdef/viewer.js")
            .unwrap();
        assert!(shim < viewer, "the shim has to run before viewer.js");
        assert!(html.contains(r#"<meta name="lucina-view" content="0123456789abcdef" />"#));
    }

    #[test]
    fn the_bundle_version_follows_the_bytes() {
        let mut files: HashMap<&'static str, Vec<u8>> = HashMap::new();
        for name in ASSET_FILES {
            files.insert(name, name.as_bytes().to_vec());
        }
        let a = bundle_hash(&files);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            a,
            bundle_hash(&files),
            "the same bytes are the same version"
        );
        files.insert("viewer.js", b"other".to_vec());
        assert_ne!(a, bundle_hash(&files), "new bytes bust the cache");
    }

    #[test]
    fn a_ticket_reissue_keeps_the_view_id_and_switches_the_mode() {
        let sb = SandboxId::Workspace;
        control().take(sb, "test");
        let epoch = control().epoch(sb);
        let id = open(sb, ViewMode::Watch, "test", epoch, "http://127.0.0.1:1");
        let before = live(&id).expect("a watch view is alive");
        assert_eq!(before.mode, ViewMode::Watch);
        let after = reissue(&before, ViewMode::Control, control().epoch(sb)).expect("reissued");
        assert_eq!(after.id, id, "the same capability, no reload");
        assert_eq!(after.mode, ViewMode::Control);
        assert!(live(&id).is_some(), "control view under the owner's lease");
        control().release(sb, "test");
        assert!(live(&id).is_none(), "and it dies with the lease, as before");
    }

    #[test]
    fn control_views_die_with_the_lease() {
        let _one = ANDROID_LEASE.blocking_lock();
        let sb = SandboxId::Android;
        control().take(sb, "test");
        let epoch = control().epoch(sb);
        let c = open(sb, ViewMode::Control, "test", epoch, "http://127.0.0.1:1");
        let w = open(sb, ViewMode::Watch, "test", epoch, "http://127.0.0.1:1");
        assert!(live(&c).is_some());
        control().release(sb, "test");
        assert!(live(&c).is_none(), "a control view ends with the takeover");
        assert!(live(&w).is_some(), "watching goes on");
        // A control view from an older takeover stays dead after a new one.
        control().take(sb, "test");
        assert!(live(&c).is_none());
        control().release(sb, "test");
        assert!(live("nope").is_none());
    }

    #[test]
    fn clipboard_messages_keep_only_text() {
        assert_eq!(clip_proto::set_response_generation(&[0x08, 0x2b]), Some(43));
        assert_eq!(clip_proto::set_response_generation(&[]), None);
        let png = [0x89, b'P', b'N', b'G'];
        let set = clip_proto::image_content(Some("ciao è"), &png, 0);
        assert_eq!(clip_proto::set_request_text(&set), Some(Some("ciao è")));
        let rewritten = clip_proto::set_request("ciao è");
        assert!(rewritten.len() < set.len(), "image and file paths are gone");
        assert_eq!(
            clip_proto::set_request_text(&rewritten),
            Some(Some("ciao è"))
        );
        let image_only = clip_proto::image_content(None, &png, 0);
        assert_eq!(clip_proto::set_request_text(&image_only), Some(None));
        assert_eq!(
            clip_proto::set_request_text(&[0x0a, 0x05, 0x0a]),
            None,
            "truncated"
        );
        assert_eq!(
            clip_proto::set_request_text(&[0x0a, 0x02, 0x0a, 0x00, 0xff]),
            None
        );
        assert_eq!(clip_proto::set_request_text(&[]), Some(None));

        let got = clip_proto::image_content(Some("hello"), &png, 300);
        assert_eq!(clip_proto::get_response(&got), Some((Some("hello"), 300)));
        let back = clip_proto::get_response_bytes(Some("hello"), 300);
        assert_eq!(clip_proto::get_response(&back), Some((Some("hello"), 300)));
        assert!(
            !back.windows(4).any(|w| w == png),
            "no image reaches the owner"
        );
        assert!(
            !back.windows(6).any(|w| w == b"secret"),
            "no file paths either"
        );
        assert_eq!(
            clip_proto::get_response(&clip_proto::get_response_bytes(None, 7)),
            Some((None, 7))
        );
    }

    const TRAILERS: &[u8] = b"grpc-status:0\r\ngrpc-message:\r\n";

    /// A stand-in sandbox speaking gRPC-web: answers `GetClipboard` with text, an
    /// image and a file path, and echoes back the message `SetClipboard` received.
    async fn fake_sandbox() -> String {
        use axum::routing::{any, post};
        let grpc = |message: &[u8]| {
            let mut body = grpc_web::frame(0, message);
            body.extend(grpc_web::frame(0x80, TRAILERS));
            ([(header::CONTENT_TYPE, "application/grpc-web+proto")], body).into_response()
        };
        let app = axum::Router::new()
            .route(
                "/cua.env.v1.ComputerService/GetClipboard",
                post(move || async move {
                    grpc(&clip_proto::image_content(
                        Some("copied on the desktop"),
                        &[0x89, b'P', b'N', b'G'],
                        42,
                    ))
                }),
            )
            .route(
                "/cua.env.v1.ComputerService/SetClipboard",
                any(move |req: Request| async move {
                    if req.method() == Method::OPTIONS {
                        return ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Vec::new())
                            .into_response();
                    }
                    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                        .await
                        .unwrap();
                    grpc(grpc_web::single_message(&body).expect("one framed message"))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn rpc(method: Method, rpc: &str, message: &[u8]) -> Request {
        let body = if method == Method::POST {
            grpc_web::frame(0, message)
        } else {
            Vec::new()
        };
        Request::builder()
            .method(method)
            .uri(format!("/x/{rpc}"))
            .header(header::CONTENT_TYPE, "application/grpc-web+proto")
            .body(Body::from(body))
            .unwrap()
    }

    /// The answer's message, and whether it ended with `grpc-status: 0`.
    async fn answer(r: Response) -> (Vec<u8>, bool) {
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let frames = grpc_web::frames(&body).expect("gRPC-web frames");
        let message = frames
            .iter()
            .find(|(f, _)| *f == 0)
            .map(|(_, m)| m.to_vec())
            .unwrap_or_default();
        (
            message,
            frames
                .iter()
                .any(|(f, t)| *f == 0x80 && grpc_web::status_ok(t)),
        )
    }

    #[test]
    fn grpc_web_frames_round_trip() {
        let mut body = grpc_web::frame(0, b"abc");
        body.extend(grpc_web::frame(0x80, TRAILERS));
        let frames = grpc_web::frames(&body).unwrap();
        assert_eq!(frames, vec![(0, &b"abc"[..]), (0x80, TRAILERS)]);
        assert!(grpc_web::status_ok(TRAILERS));
        assert!(!grpc_web::status_ok(b"grpc-status:7\r\n"));
        assert_eq!(
            grpc_web::single_message(&grpc_web::frame(0, b"m")),
            Some(&b"m"[..])
        );
        assert_eq!(
            grpc_web::single_message(&grpc_web::frame(1, b"m")),
            None,
            "compressed"
        );
        assert_eq!(
            grpc_web::single_message(&body),
            None,
            "a request is one frame"
        );
        assert_eq!(grpc_web::frames(&[0, 0, 0, 0, 9, 1]), None, "truncated");
    }

    #[tokio::test]
    async fn the_clipboard_syncs_text_only_and_only_while_driving() {
        let machine = fake_sandbox().await;
        let _one = ANDROID_LEASE.lock().await;
        let sb = SandboxId::Android;
        let watch = open(
            sb,
            ViewMode::Watch,
            "clip-test",
            control().epoch(sb),
            &machine,
        );
        let get = "cua.env.v1.ComputerService/GetClipboard";
        let set = "cua.env.v1.ComputerService/SetClipboard";
        for (m, path) in [
            (Method::POST, get),
            (Method::POST, set),
            (Method::OPTIONS, set),
        ] {
            let r = serve(&watch, path, rpc(m.clone(), path, &[]), true).await;
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "watching: {m} {path}");
        }

        control().take(sb, "clip-test");
        let ctl = open(
            sb,
            ViewMode::Control,
            "clip-test",
            control().epoch(sb),
            &machine,
        );
        let r = serve(&ctl, set, rpc(Method::OPTIONS, set, &[]), true).await;
        assert_eq!(r.status(), StatusCode::OK, "the CORS preflight passes");

        let r = serve(&ctl, get, rpc(Method::POST, get, &[]), true).await;
        assert_eq!(r.status(), StatusCode::OK);
        let (message, ok) = answer(r).await;
        assert!(ok, "the trailers come through");
        assert_eq!(
            clip_proto::get_response(&message),
            Some((Some("copied on the desktop"), 42))
        );
        assert!(
            !message.windows(3).any(|w| w == b"PNG"),
            "the image is dropped"
        );

        let paste =
            clip_proto::image_content(Some("pasted from the Mac"), &[0x89, b'P', b'N', b'G'], 0);
        let r = serve(&ctl, set, rpc(Method::POST, set, &paste), true).await;
        assert_eq!(r.status(), StatusCode::OK);
        let (echoed, ok) = answer(r).await;
        assert!(ok);
        assert_eq!(
            echoed,
            clip_proto::set_request("pasted from the Mac"),
            "the desktop gets the text only"
        );

        // Refusals are gRPC-web errors the viewer's opaque origin can read.
        let image_only = clip_proto::image_content(None, &[1, 2, 3], 0);
        let huge = clip_proto::set_request(&"x".repeat(CLIP_TEXT_LIMIT + 1));
        for (body, code) in [(image_only, "12"), (huge, "8")] {
            let r = serve(&ctl, set, rpc(Method::POST, set, &body), true).await;
            assert_eq!(r.status(), StatusCode::OK);
            assert_eq!(r.headers()["grpc-status"], code);
            assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
            let why = r.headers()["grpc-message"]
                .to_str()
                .unwrap()
                .to_ascii_lowercase();
            assert!(
                !why.contains("permission") && !why.contains("not allowed"),
                "{why}"
            );
        }
        let mut json = rpc(Method::POST, get, &[]);
        json.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/grpc-web+json"),
        );
        assert_eq!(
            serve(&ctl, get, json, true).await.headers()["grpc-status"],
            "12"
        );

        // The home-network door reaches it too (same lease rules).
        let r = serve_capability(&ctl, get, rpc(Method::POST, get, &[])).await;
        assert_eq!(r.status(), StatusCode::OK);

        control().release(sb, "clip-test");
        let r = serve(&ctl, get, rpc(Method::POST, get, &[]), true).await;
        assert_eq!(
            r.status(),
            StatusCode::GONE,
            "the clipboard goes with the lease"
        );
    }

    #[tokio::test]
    async fn the_socket_is_spliced_byte_for_byte() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        });
        let upstream = TcpStream::connect(addr).await.unwrap();
        let (mut ours, theirs) = tokio::io::duplex(64);
        let view = View {
            id: "t".into(),
            sandbox: SandboxId::Workspace,
            mode: ViewMode::Watch,
            device: "t".into(),
            epoch: 0,
            machine: format!("http://{addr}"),
            expires: Instant::now() + Duration::from_secs(5),
        };
        let task = tokio::spawn(splice(view, theirs, upstream, b"hi".to_vec()));
        let mut got = [0u8; 2];
        ours.read_exact(&mut got).await.unwrap();
        assert_eq!(
            &got, b"hi",
            "bytes after the 101 head reach the viewer first"
        );
        ours.write_all(b"ping").await.unwrap();
        let mut echo = [0u8; 4];
        ours.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"ping");
        drop(ours);
        task.await.unwrap();
    }
}
