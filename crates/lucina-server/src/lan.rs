//! The home-network door to Lucina's live view (owner only).
//!
//! At home the owner's iPhone or Mac would otherwise watch the desktop through
//! phone → p1 → nezopt and back. This serves the same view directly from nezopt on
//! the LAN, over HTTPS with a real certificate, so the browser treats it like p1:
//!
//! - **Where:** `LUCINA_LAN_BIND` (e.g. `0.0.0.0:9443`), reached as `LUCINA_LAN_URL`
//!   (`https://lan.getsecretary.tech:9443`; an A record to nezopt's LAN address, DNS only).
//!   The certificate is `LUCINA_LAN_CERT_DIR/{fullchain.pem,key.pem}` (default
//!   `~/.config/lucina-rs/lan`), issued with DNS-01 and renewed by `tools/lan-cert`; it is
//!   re-read when the file changes, so a renewal needs no restart.
//! - **What:** only the viewer surface, under the same paths as on p1 so the view's
//!   relative URLs work unchanged:
//!   `ANY /v1/computer/view/{id}/{*rest}` (the view; its id and ticket are the capability,
//!   exactly as on p1; never the owner-only ticket re-issue), `GET
//!   /v1/computer/viewer-assets/{version}/{file}`, `GET /v1/computer/lan-probe` for the
//!   page's reachability probe, and `GET /v1/computer/desk/ws`, LucinaDesk's socket
//!   (`crate::desk`; its single-use ticket is the capability, from the page origin only).
//!   Everything else is 404.
//! - **Who:** secretary-server offers the URL (`GET /api/secretary/computer/lan` gives it the
//!   URL and the house's public IP) only to the owner, and only when the request comes from
//!   that public IP; the page probes it and falls back to p1 silently.
//! - **Headers:** the view's own (`harden`), except `frame-ancestors`, which names the page
//!   origin (`LUCINA_LAN_PAGE_ORIGIN`, default `https://getsecretary.tech`) since the frame is
//!   now cross-origin. Chromium's Private Network Access preflights (a public page reaching a
//!   private address) are answered for that origin only.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use hyper_util::rt::TokioIo;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// How long the house's public IP is trusted before it is looked up again.
const PUBLIC_IP_TTL: Duration = Duration::from_secs(300);
const PNA_ALLOW: HeaderName = HeaderName::from_static("access-control-allow-private-network");
const PNA_REQUEST: HeaderName = HeaderName::from_static("access-control-request-private-network");

#[derive(Debug, Clone)]
pub struct LanConfig {
    pub bind: SocketAddr,
    /// `https://lan.getsecretary.tech:9443`, no trailing slash.
    pub url: String,
    pub cert_dir: PathBuf,
    /// The page that frames the view (`https://getsecretary.tech`).
    pub page_origin: String,
}

impl LanConfig {
    fn from_env() -> Option<Self> {
        let text = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let bind = text("LUCINA_LAN_BIND")?.parse().map_err(|e| tracing::warn!(error = %e, "LUCINA_LAN_BIND")).ok()?;
        let url = text("LUCINA_LAN_URL")?.trim_end_matches('/').to_owned();
        if !url.starts_with("https://") {
            tracing::warn!("LUCINA_LAN_URL must be https://; the home-network door stays closed");
            return None;
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into());
        Some(Self {
            bind,
            url,
            cert_dir: text("LUCINA_LAN_CERT_DIR").map_or_else(|| PathBuf::from(home).join(".config/lucina-rs/lan"), PathBuf::from),
            page_origin: text("LUCINA_LAN_PAGE_ORIGIN").unwrap_or_else(|| "https://getsecretary.tech".into()),
        })
    }
}

/// The door's configuration, or `None` when it is not set up (the default).
pub fn config() -> Option<&'static LanConfig> {
    static C: OnceLock<Option<LanConfig>> = OnceLock::new();
    C.get_or_init(LanConfig::from_env).as_ref()
}

// ---------------------------------------------------------------- certificate

/// The certificate files, re-read when `fullchain.pem` changes (a stat per handshake).
#[derive(Debug)]
struct CertFiles {
    dir: PathBuf,
    current: Mutex<Option<(SystemTime, Arc<CertifiedKey>)>>,
}

impl CertFiles {
    fn load(&self) -> Result<Arc<CertifiedKey>, String> {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(self.dir.join("fullchain.pem"))
            .and_then(|it| it.collect())
            .map_err(|e| format!("fullchain.pem: {e}"))?;
        if chain.is_empty() {
            return Err("fullchain.pem has no certificate".into());
        }
        let key = PrivateKeyDer::from_pem_file(self.dir.join("key.pem")).map_err(|e| format!("key.pem: {e}"))?;
        let signer = rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| format!("key.pem: {e}"))?;
        Ok(Arc::new(CertifiedKey::new(chain, signer)))
    }

    fn current(&self) -> Option<Arc<CertifiedKey>> {
        let mtime = std::fs::metadata(self.dir.join("fullchain.pem")).and_then(|m| m.modified()).ok();
        let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(m), Some((seen, key))) = (mtime, cur.as_ref()) {
            if *seen == m {
                return Some(key.clone());
            }
        }
        match self.load() {
            Ok(key) => {
                tracing::info!("home-network door: certificate loaded");
                *cur = Some((mtime.unwrap_or(SystemTime::UNIX_EPOCH), key.clone()));
                Some(key)
            }
            Err(e) => {
                tracing::warn!(error = %e, "home-network door: certificate unreadable");
                cur.as_ref().map(|(_, k)| k.clone())
            }
        }
    }
}

impl ResolvesServerCert for CertFiles {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current()
    }
}

/// Whether the door can actually serve (configured, certificate readable).
pub fn ready() -> bool {
    config().is_some_and(|c| CertFiles { dir: c.cert_dir.clone(), current: Mutex::new(None) }.load().is_ok())
}

// ---------------------------------------------------------------- the house's public IP

fn ip_slot() -> &'static Mutex<Option<(Instant, String)>> {
    static S: OnceLock<Mutex<Option<(Instant, String)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// `ip=…` from Cloudflare's trace page: the address the internet sees this house as.
fn parse_trace(text: &str) -> Option<String> {
    let ip = text.lines().find_map(|l| l.strip_prefix("ip="))?.trim();
    ip.parse::<std::net::IpAddr>().ok().map(|a| a.to_string())
}

/// The house's public IP (looked up at most every five minutes), or `None` offline.
pub async fn public_ip() -> Option<String> {
    if let Some((at, ip)) = ip_slot().lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if at.elapsed() < PUBLIC_IP_TTL {
            return Some(ip);
        }
    }
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    let http = HTTP.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(5)).build().expect("lan http client"));
    let text = http.get("https://1.1.1.1/cdn-cgi/trace").send().await.ok()?.text().await.ok()?;
    let ip = parse_trace(&text)?;
    *ip_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), ip.clone()));
    Some(ip)
}

// ---------------------------------------------------------------- the router

/// The view's CSP, with the page origin allowed to frame it.
fn view_csp(page_origin: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("sandbox allow-scripts allow-pointer-lock; frame-ancestors {page_origin}"))
        .unwrap_or_else(|_| HeaderValue::from_static(crate::computer_view::VIEW_CSP))
}

/// CORS for the page origin, plus the Private Network Access answer.
fn allow_page(h: &mut HeaderMap, page_origin: &str) {
    if let Ok(v) = HeaderValue::from_str(page_origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    h.insert(PNA_ALLOW, HeaderValue::from_static("true"));
    h.insert(header::VARY, HeaderValue::from_static("origin"));
}

fn from_page(h: &HeaderMap, page_origin: &str) -> bool {
    h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(page_origin)
}

/// The page origin, shared by the handlers.
type Page = Arc<str>;

/// Chromium asks before a public page reaches a private address (the probe and the
/// frame's navigation): `OPTIONS` + `Access-Control-Request-Private-Network`.
/// Answered for the page origin only; any other `OPTIONS` goes on to the routes.
async fn private_network_preflight(State(page): State<Page>, req: Request, next: Next) -> Response {
    if req.method() == Method::OPTIONS && req.headers().contains_key(PNA_REQUEST) {
        if !from_page(req.headers(), &page) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let mut resp = StatusCode::NO_CONTENT.into_response();
        allow_page(resp.headers_mut(), &page);
        resp.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET"));
        resp.headers_mut().insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        return resp;
    }
    next.run(req).await
}

/// `GET /v1/computer/lan-probe`: the page's "is the door reachable from here" check.
async fn probe(State(page): State<Page>, req: Request) -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if from_page(req.headers(), &page) {
        allow_page(resp.headers_mut(), &page);
    }
    resp
}

async fn view(State(page): State<Page>, Path((view_id, rest)): Path<(String, String)>, req: Request) -> Response {
    let mut resp = crate::computer_view::serve_capability(&view_id, &rest, req).await;
    resp.headers_mut().insert(header::CONTENT_SECURITY_POLICY, view_csp(&page));
    resp
}

async fn asset(Path((version, file)): Path<(String, String)>) -> Response {
    crate::computer_view::asset(&version, &file).await
}

/// `GET /v1/computer/desk/ws`: LucinaDesk's socket (`crate::desk`), from the page only.
async fn desk(State(page): State<Page>, req: Request) -> Response {
    if !from_page(req.headers(), &page) {
        return StatusCode::FORBIDDEN.into_response();
    }
    crate::desk::upgrade(req, "lan").await
}

/// Only the viewer surface; everything else is 404.
pub fn router(page_origin: &str) -> Router {
    let page: Page = Arc::from(page_origin);
    Router::new()
        .route("/v1/computer/lan-probe", get(probe))
        .route("/v1/computer/view/{view_id}/{*rest}", any(view))
        .route("/v1/computer/viewer-assets/{version}/{file}", get(asset))
        .route("/v1/computer/desk/ws", get(desk))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn_with_state(page.clone(), private_network_preflight))
        .with_state(page)
}

// ---------------------------------------------------------------- the listener

/// Serves the door until `stop` turns true. HTTP/1.1 only (the media WebSocket upgrades on it).
pub async fn serve(cfg: &'static LanConfig, mut stop: tokio::sync::watch::Receiver<bool>) -> std::io::Result<()> {
    let certs = Arc::new(CertFiles { dir: cfg.cert_dir.clone(), current: Mutex::new(None) });
    if certs.current().is_none() {
        tracing::warn!(dir = %cfg.cert_dir.display(), "home-network door: no certificate yet (run lan-cert); serving once it appears");
    }
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_no_client_auth()
        .with_cert_resolver(certs);
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind(cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, url = %cfg.url, "home-network door listening");
    let app = router(&cfg.page_origin);
    loop {
        let (tcp, _) = tokio::select! {
            // (mapped to a bool: the watch guard must not live across an await)
            _ = async { stop.wait_for(|s| *s).await.is_ok() } => return Ok(()),
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "home-network door: accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await else {
                return;
            };
            let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let app = app.clone();
                async move { tower::ServiceExt::oneshot(app, req.map(Body::new)).await }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .with_upgrades()
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_computer_use::{control, SandboxId, ViewMode};

    async fn call(app: &Router, method: &str, path: &str, origin: Option<&str>, pna: bool) -> Response {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(o) = origin {
            req = req.header(header::ORIGIN, o);
        }
        if pna {
            req = req.header(PNA_REQUEST, "true");
        }
        tower::ServiceExt::oneshot(app.clone(), req.body(Body::empty()).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn the_door_serves_only_the_viewer_surface() {
        let page = "https://getsecretary.tech";
        let app = router(page);
        // Nothing of Lucina's API, not even health.
        for path in ["/", "/health", "/api/secretary/computer", "/api/secretary/computer/view/x/viewer/", "/v1/computer/control", "/v1/computer/view"] {
            assert_eq!(call(&app, "GET", path, None, false).await.status(), StatusCode::NOT_FOUND, "{path}");
        }
        // A live view: its viewer is served, its ticket re-issue never is (that is the owner's, on p1).
        let sb = SandboxId::Workspace;
        let id = crate::computer_view::open(sb, ViewMode::Watch, "lan-test", control().epoch(sb), "http://127.0.0.1:1");
        let r = call(&app, "POST", &format!("/v1/computer/view/{id}/ticket"), Some(page), false).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(r.headers()[header::CONTENT_SECURITY_POLICY], "sandbox allow-scripts allow-pointer-lock; frame-ancestors https://getsecretary.tech");
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        for rest in ["files", "cua.env.v1.SystemService/CreateViewerTicket", "cua.env.v1.FilesService/Read"] {
            assert_eq!(call(&app, "POST", &format!("/v1/computer/view/{id}/{rest}"), None, false).await.status(), StatusCode::FORBIDDEN, "{rest}");
        }
        // An unknown view is gone, like on p1.
        assert_eq!(call(&app, "GET", &format!("/v1/computer/view/{}/viewer/", "a".repeat(64)), None, false).await.status(), StatusCode::GONE);
        // Asset names outside the bundle are 404 before anything is fetched.
        assert_eq!(call(&app, "GET", "/v1/computer/viewer-assets/0123456789abcdef/index.html", None, false).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_probe_and_private_network_preflight_answer_the_page_only() {
        let page = "https://getsecretary.tech";
        let app = router(page);
        let r = call(&app, "GET", "/v1/computer/lan-probe", Some(page), false).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], page);
        let r = call(&app, "GET", "/v1/computer/lan-probe", Some("https://evil.example"), false).await;
        assert!(r.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(), "no CORS for another site");
        let r = call(&app, "OPTIONS", "/v1/computer/lan-probe", Some(page), true).await;
        assert_eq!((r.status(), r.headers()[PNA_ALLOW].clone()), (StatusCode::NO_CONTENT, HeaderValue::from_static("true")));
        let r = call(&app, "OPTIONS", "/v1/computer/view/x/viewer/", Some(page), true).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "the frame's navigation preflight");
        let r = call(&app, "OPTIONS", "/v1/computer/lan-probe", Some("https://evil.example"), true).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_desk_socket_is_open_to_the_page_only() {
        let page = "https://getsecretary.tech";
        let app = router(page);
        for origin in [None, Some("https://evil.example")] {
            assert_eq!(call(&app, "GET", "/v1/computer/desk/ws", origin, false).await.status(), StatusCode::FORBIDDEN, "{origin:?}");
        }
        // From the page it reaches the desk, which wants a real WebSocket upgrade (and then a ticket).
        let r = call(&app, "GET", "/v1/computer/desk/ws", Some(page), false).await;
        assert!(r.status().is_client_error() && r.status() != StatusCode::FORBIDDEN && r.status() != StatusCode::NOT_FOUND, "{}", r.status());
    }

    #[test]
    fn the_public_ip_comes_from_the_trace_page() {
        let trace = "fl=1\nh=1.1.1.1\nip=84.238.78.250\nts=1\n";
        assert_eq!(parse_trace(trace).as_deref(), Some("84.238.78.250"));
        assert_eq!(parse_trace("ip=not-an-ip\n"), None);
        assert_eq!(parse_trace("h=1.1.1.1\n"), None);
    }

    #[test]
    fn only_the_page_origin_is_allowed_and_may_frame_the_view() {
        let page = "https://getsecretary.tech";
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, HeaderValue::from_static("https://getsecretary.tech"));
        assert!(from_page(&h, page));
        h.insert(header::ORIGIN, HeaderValue::from_static("https://evil.example"));
        assert!(!from_page(&h, page));
        assert!(!from_page(&HeaderMap::new(), page));
        assert_eq!(view_csp(page), "sandbox allow-scripts allow-pointer-lock; frame-ancestors https://getsecretary.tech");
        let mut h = HeaderMap::new();
        allow_page(&mut h, page);
        assert_eq!(h[header::ACCESS_CONTROL_ALLOW_ORIGIN], page);
        assert_eq!(h[PNA_ALLOW], "true");
    }
}
