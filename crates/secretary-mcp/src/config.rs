//! Configuration from the environment.
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `SECRETARY_MCP_BIND` | `127.0.0.1:8810` | listen address (Caddy in front) |
//! | `SECRETARY_MCP_PUBLIC_URL` | `https://getsecretary.tech` | OAuth issuer; the MCP resource is `<this>/mcp` |
//! | `SECRETARY_MCP_DATA_DIR` | `/var/lib/secretary-mcp` | `tokens.json`, `oauth.json` (0600) |
//! | `SECRETARY_MCP_BACKEND_URL` | `http://127.0.0.1:8800` | secretary-server |
//! | `SECRETARY_MCP_BACKEND_TOKEN_FILE` | unset | a secretary-server device token (`sec_…`), one line |
//! | `SECRETARY_MCP_SERVICE_TOKEN_FILE` | unset | the secretary-server service token (`svc_…`) for `/v1/insights/summary` |
//! | `SECRETARY_MCP_TIMEZONE` | `Europe/Rome` | for gym/body summaries and timeline times |
//! | `SECRETARY_MCP_TRUST_PROXY` | `0` | `1`: client IP from the right-most `X-Forwarded-For` |
//! | `SECRETARY_MCP_OAUTH_REDIRECTS` | ChatGPT's two | allowed `redirect_uri`s: exact URLs, or prefixes ending in `/` |
//! | `SECRETARY_MCP_OAUTH_CIMD` | `https://chatgpt.com/oauth/` | allowed client-id metadata document URL prefixes (empty: CIMD off) |
//! | `SECRETARY_MCP_ALLOWED_HOSTS` | public host, `localhost`, `127.0.0.1` | inbound `Host` check of `/mcp` |
//! | `SECRETARY_MCP_ALLOWED_ORIGINS` | public origin, `https://chatgpt.com`, `https://claude.ai` | `Origin` check of `/mcp` (requests without `Origin` pass) |
//! | `SECRETARY_MCP_ACCESS_TTL_SECS` | `3600` | OAuth access token lifetime |
//! | `SECRETARY_MCP_REFRESH_TTL_SECS` | `2592000` (30 d) | OAuth refresh token lifetime |
//! | `SECRETARY_MCP_DIAGNOSTICS_URL` | unset | lucina-diagnostics on nezopt (`http://100.107.164.33:8850`) for the `diagnostics_*` tools |
//! | `SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE` | unset | its service token (`~/.config/lucina-diagnostics/service.token` on nezopt), one line |
//! | `SECRETARY_MCP_STATUS_TOKEN_FILE` | unset | enables `GET /internal/status` (AI clients for the app's Links tab); same content as secretary-server's `SECRETARY_MCP_STATUS_TOKEN_FILE` |

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::Url;

/// ChatGPT's connector OAuth callbacks (developers.openai.com/apps-sdk/build/auth):
/// the stable one (used when the authorization server returns `iss`, RFC 9207)
/// and the per-connector fallback `https://chatgpt.com/connector/oauth/{callback_id}`.
pub const CHATGPT_REDIRECTS: [&str; 2] = [
    "https://chatgpt.com/connector_platform_oauth_redirect",
    "https://chatgpt.com/connector/oauth/",
];

/// ChatGPT's client-id metadata documents live under this prefix
/// (`https://chatgpt.com/oauth/client.json`, `…/oauth/{callback_id}/client.json`).
pub const CHATGPT_CIMD_PREFIX: &str = "https://chatgpt.com/oauth/";

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// Issuer, no trailing slash (`https://getsecretary.tech`).
    pub public_url: String,
    pub data_dir: PathBuf,
    pub backend_url: String,
    pub backend_token: Option<String>,
    pub service_token: Option<String>,
    pub timezone: chrono_tz::Tz,
    pub trust_proxy: bool,
    pub redirect_allow: Vec<String>,
    pub cimd_prefixes: Vec<String>,
    pub allowed_hosts: Vec<String>,
    /// `scheme://host:port` entries (`:*` for any port).
    pub allowed_origins: Vec<String>,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    /// Bearer for `GET /internal/status`; `None`: the route answers 404.
    pub status_token: Option<String>,
    /// lucina-diagnostics base URL and service token (`diagnostics_*`).
    pub diagnostics_url: Option<String>,
    pub diagnostics_token: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("public_url", &self.public_url)
            .field("data_dir", &self.data_dir)
            .field("backend_url", &self.backend_url)
            .field(
                "backend_token",
                &self.backend_token.as_ref().map(|_| "<set>"),
            )
            .field(
                "service_token",
                &self.service_token.as_ref().map(|_| "<set>"),
            )
            .field("timezone", &self.timezone)
            .field("trust_proxy", &self.trust_proxy)
            .field("redirect_allow", &self.redirect_allow)
            .field("cimd_prefixes", &self.cimd_prefixes)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("allowed_origins", &self.allowed_origins)
            .field("diagnostics_url", &self.diagnostics_url)
            .field(
                "diagnostics_token",
                &self.diagnostics_token.as_ref().map(|_| "<set>"),
            )
            .finish()
    }
}

impl Config {
    /// Defaults for a public URL (tests override the rest).
    pub fn new(public_url: &str, data_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let public_url = normalize_public_url(public_url)?;
        let host = Url::parse(&public_url)?
            .host_str()
            .unwrap_or_default()
            .to_owned();
        let allowed_origins = vec![
            origin_with_port(&public_url),
            "https://chatgpt.com:443".into(),
            "https://claude.ai:443".into(),
        ];
        Ok(Self {
            bind: "127.0.0.1:8810".parse()?,
            public_url,
            data_dir: data_dir.into(),
            backend_url: "http://127.0.0.1:8800".into(),
            backend_token: None,
            service_token: None,
            timezone: chrono_tz::Europe::Rome,
            trust_proxy: false,
            redirect_allow: CHATGPT_REDIRECTS.iter().map(|s| (*s).to_owned()).collect(),
            cimd_prefixes: vec![CHATGPT_CIMD_PREFIX.to_owned()],
            allowed_origins,
            allowed_hosts: vec![host, "localhost".into(), "127.0.0.1".into(), "::1".into()],
            access_ttl: Duration::from_secs(3600),
            refresh_ttl: Duration::from_secs(30 * 24 * 3600),
            status_token: None,
            diagnostics_url: None,
            diagnostics_token: None,
        })
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let mut cfg = Self::new(
            &get("SECRETARY_MCP_PUBLIC_URL").unwrap_or_else(|| "https://getsecretary.tech".into()),
            get("SECRETARY_MCP_DATA_DIR").unwrap_or_else(|| "/var/lib/secretary-mcp".into()),
        )?;
        if let Some(b) = get("SECRETARY_MCP_BIND") {
            cfg.bind = b.parse().context("SECRETARY_MCP_BIND")?;
        }
        if let Some(u) = get("SECRETARY_MCP_BACKEND_URL") {
            cfg.backend_url = u.trim_end_matches('/').to_owned();
        }
        if let Some(p) = get("SECRETARY_MCP_BACKEND_TOKEN_FILE") {
            cfg.backend_token = Some(read_secret(&p, "SECRETARY_MCP_BACKEND_TOKEN_FILE")?);
        }
        if let Some(p) = get("SECRETARY_MCP_SERVICE_TOKEN_FILE") {
            cfg.service_token = Some(read_secret(&p, "SECRETARY_MCP_SERVICE_TOKEN_FILE")?);
        }
        if let Some(p) = get("SECRETARY_MCP_STATUS_TOKEN_FILE") {
            cfg.status_token = Some(read_secret(&p, "SECRETARY_MCP_STATUS_TOKEN_FILE")?);
        }
        if let Some(u) = get("SECRETARY_MCP_DIAGNOSTICS_URL") {
            cfg.diagnostics_url = Some(u.trim().trim_end_matches('/').to_owned());
        }
        if let Some(p) = get("SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE") {
            cfg.diagnostics_token = Some(read_secret(&p, "SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE")?);
        }
        if let Some(tz) = get("SECRETARY_MCP_TIMEZONE") {
            cfg.timezone = tz
                .parse()
                .map_err(|_| anyhow::anyhow!("SECRETARY_MCP_TIMEZONE: unknown time zone {tz:?}"))?;
        }
        cfg.trust_proxy = match get("SECRETARY_MCP_TRUST_PROXY").as_deref() {
            None | Some("0") | Some("false") => false,
            Some("1") | Some("true") => true,
            Some(v) => bail!("SECRETARY_MCP_TRUST_PROXY must be 0 or 1, got {v:?}"),
        };
        if let Some(list) = get("SECRETARY_MCP_OAUTH_REDIRECTS") {
            cfg.redirect_allow = split_list(&list);
        }
        if let Ok(list) = std::env::var("SECRETARY_MCP_OAUTH_CIMD") {
            cfg.cimd_prefixes = split_list(&list);
        }
        if let Some(list) = get("SECRETARY_MCP_ALLOWED_HOSTS") {
            cfg.allowed_hosts = split_list(&list);
        }
        if let Some(list) = get("SECRETARY_MCP_ALLOWED_ORIGINS") {
            cfg.allowed_origins = split_list(&list);
        }
        if let Some(s) = get("SECRETARY_MCP_ACCESS_TTL_SECS") {
            cfg.access_ttl =
                Duration::from_secs(s.parse().context("SECRETARY_MCP_ACCESS_TTL_SECS")?);
        }
        if let Some(s) = get("SECRETARY_MCP_REFRESH_TTL_SECS") {
            cfg.refresh_ttl =
                Duration::from_secs(s.parse().context("SECRETARY_MCP_REFRESH_TTL_SECS")?);
        }
        for p in &cfg.cimd_prefixes {
            if !p.ends_with('/') {
                bail!("SECRETARY_MCP_OAUTH_CIMD entries must end with '/': {p:?}");
            }
        }
        Ok(cfg)
    }

    /// The protected resource (RFC 8707 / RFC 9728): `<public>/mcp`.
    pub fn resource(&self) -> String {
        format!("{}/mcp", self.public_url)
    }

    /// RFC 9728 metadata URL for the resource (path-inserted form).
    pub fn resource_metadata_url(&self) -> String {
        format!(
            "{}/.well-known/oauth-protected-resource/mcp",
            self.public_url
        )
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.public_url)
    }
}

fn normalize_public_url(s: &str) -> anyhow::Result<String> {
    let url = Url::parse(s.trim()).with_context(|| format!("bad public URL {s:?}"))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        bail!("public URL must be http(s)://host: {s:?}");
    }
    if url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
        bail!("public URL must be an origin without path: {s:?}");
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// `https://host` → `https://host:443` (rmcp matches origins with ports).
pub fn origin_with_port(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => format!(
            "{}://{}:{}",
            u.scheme(),
            u.host_str().unwrap_or_default(),
            u.port_or_known_default().unwrap_or(443)
        ),
        Err(_) => url.to_owned(),
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn read_secret(path: &str, var: &str) -> anyhow::Result<String> {
    let s = std::fs::read_to_string(path).with_context(|| format!("{var}: cannot read {path}"))?;
    let s = s.trim().to_owned();
    if s.is_empty() || s.contains(char::is_whitespace) {
        bail!("{var}: {path} must hold exactly one token");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let c = Config::new("https://getsecretary.tech/", "/tmp/x").unwrap();
        assert_eq!(c.public_url, "https://getsecretary.tech");
        assert_eq!(c.resource(), "https://getsecretary.tech/mcp");
        assert_eq!(
            c.resource_metadata_url(),
            "https://getsecretary.tech/.well-known/oauth-protected-resource/mcp"
        );
        assert!(c.allowed_hosts.contains(&"getsecretary.tech".to_string()));
        assert_eq!(c.allowed_origins[0], "https://getsecretary.tech:443");
        assert!(Config::new("https://x.tech/mcp", "/tmp").is_err());
        assert!(Config::new("ftp://x.tech", "/tmp").is_err());
    }
}
