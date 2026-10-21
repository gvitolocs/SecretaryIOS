//! The service token: how Lucina (a server, not a device) reads
//! `/v1/insights/summary`.
//!
//! - One token, `svc_` + base64url(32 random bytes), in the file named by
//!   `SECRETARY_SERVICE_TOKEN_FILE`; only its SHA-256 is kept in memory and it
//!   is compared in constant time.
//! - It is accepted on the insights route only. Device tokens and cookies are
//!   refused there, and device routes refuse it (their parser only accepts
//!   `sec_` tokens).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{bail, Context};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::AppState;

pub const SERVICE_PREFIX: &str = "svc_";
/// `svc_` + at least 32 characters.
pub const MIN_LEN: usize = 36;
/// Rate-limit bucket shared by every service-token request.
pub const RATE_KEY: &str = "service";

#[derive(Clone)]
pub struct ServiceToken {
    hash: [u8; 32],
}

impl std::fmt::Debug for ServiceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceToken(<set>)")
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl ServiceToken {
    pub fn from_secret(token: &str) -> anyhow::Result<Self> {
        let token = token.trim();
        if !token.starts_with(SERVICE_PREFIX) || token.len() < MIN_LEN {
            bail!("a service token must start with {SERVICE_PREFIX} and be at least {MIN_LEN} characters");
        }
        Ok(Self {
            hash: digest(token),
        })
    }

    /// Read `SECRETARY_SERVICE_TOKEN_FILE`. Never includes the content in errors.
    pub fn read_file(path: &str) -> anyhow::Result<Self> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("SECRETARY_SERVICE_TOKEN_FILE {path}"))?;
        if meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(
                path,
                "service token file is readable by group/others; chmod 600 it"
            );
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading SECRETARY_SERVICE_TOKEN_FILE {path}"))?;
        if raw.trim().is_empty() {
            bail!("SECRETARY_SERVICE_TOKEN_FILE {path} is empty");
        }
        Self::from_secret(&raw).with_context(|| format!("SECRETARY_SERVICE_TOKEN_FILE {path}"))
    }

    pub fn matches(&self, token: &str) -> bool {
        bool::from(digest(token).ct_eq(&self.hash))
    }
}

/// A fresh service token.
pub fn generate() -> anyhow::Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    Ok(format!("{SERVICE_PREFIX}{}", URL_SAFE_NO_PAD.encode(buf)))
}

/// Write a fresh token to `path` (mode 0600). Refuses to replace an existing
/// file unless `force`.
pub fn write_new(path: &Path, force: bool) -> anyhow::Result<()> {
    let token = generate()?;
    if path.exists() && !force {
        bail!("{} exists; pass --force to replace it", path.display());
    }
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        writeln!(f, "{token}")?;
        f.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        if let Some(d) = dir {
            if let Ok(d) = std::fs::File::open(d) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn service_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .filter(|t| t.starts_with(SERVICE_PREFIX))
}

/// Middleware for the service-only routes.
pub async fn require_service(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(expected) = &st.service_token else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_token_not_configured",
            "SECRETARY_SERVICE_TOKEN_FILE is not set on the server",
        ));
    };
    match service_bearer(req.headers()) {
        Some(token) if expected.matches(token) => {}
        Some(_) => return Err(ApiError::unauthorized("unknown service token")),
        None => return Err(ApiError::unauthorized("this route needs a service token")),
    }
    if let Err(wait) = st.limiter.check(RATE_KEY) {
        return Err(ApiError::rate_limited(wait));
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_format_hashing_and_file_io() {
        let t = generate().unwrap();
        assert!(t.starts_with(SERVICE_PREFIX) && t.len() == 4 + 43);
        let s = ServiceToken::from_secret(&format!("  {t}\n")).unwrap();
        assert!(s.matches(&t));
        assert!(!s.matches(&generate().unwrap()));
        assert!(!format!("{s:?}").contains(&t[4..]));
        assert!(ServiceToken::from_secret("svc_short").is_err());
        assert!(ServiceToken::from_secret(&t.replacen("svc_", "sec_", 1)).is_err());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.token");
        write_new(&path, false).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(ServiceToken::read_file(path.to_str().unwrap())
            .unwrap()
            .matches(first.trim()));
        assert!(write_new(&path, false).is_err());
        write_new(&path, true).unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), first);
        std::fs::write(&path, "\n").unwrap();
        assert!(ServiceToken::read_file(path.to_str().unwrap()).is_err());
    }
}
