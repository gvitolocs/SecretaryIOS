//! Pixiv app PKCE. Errors deliberately carry no upstream text or credentials.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use md5::Md5;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fmt,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

pub const CLIENT_ID: &str = "MOBrBDS8blbauoSck0ZfDbtuzpyT";
pub const CLIENT_SECRET: &str = "lsACyCD94FhDUtGTXi3QzcFE2uU1hqtDaKeqrdwj";
pub const HASH_SECRET: &str = "28c1fdd170a5204386cb1313c7077b34f83e4aaf4aa829ce78c231e05b0bae2c";
pub const REDIRECT_URI: &str = "https://app-api.pixiv.net/web/v1/users/auth/pixiv/callback";
const UA: &str = "PixivAndroidApp/5.0.234 (Android 11; Pixel 5)";

#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);
impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}
impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Debug, Clone)]
pub struct PixivUser {
    pub id: String,
    pub name: String,
    pub account: String,
    pub premium: bool,
    pub x_restrict: i64,
}
#[derive(Debug)]
pub struct TokenSet {
    pub access: Secret,
    pub refresh: Secret,
    pub expires_at: Instant,
    pub user: PixivUser,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("token rejected")]
    Rejected,
    #[error("token endpoint unavailable")]
    Unavailable,
    #[error("token endpoint failed")]
    Other,
}
#[derive(Debug, Clone)]
pub struct Config {
    pub token_url: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            token_url: "https://oauth.secure.pixiv.net/auth/token".into(),
        }
    }
}

pub fn random_hex() -> Result<String, TokenError> {
    let mut b = [0; 16];
    getrandom::fill(&mut b).map_err(|_| TokenError::Other)?;
    Ok(hex::encode(b))
}
pub fn verifier() -> Result<Secret, TokenError> {
    let mut b = Zeroizing::new([0; 32]);
    getrandom::fill(&mut *b).map_err(|_| TokenError::Other)?;
    Ok(Secret::new(URL_SAFE_NO_PAD.encode(*b)))
}
pub fn challenge(v: &Secret) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(v.expose().as_bytes()))
}
pub fn login_url(v: &Secret) -> String {
    format!("https://app-api.pixiv.net/web/v1/login?code_challenge={}&code_challenge_method=S256&client=pixiv-android", challenge(v))
}
fn headers() -> reqwest::header::HeaderMap {
    let time = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string();
    let hash = hex::encode(Md5::digest(format!("{time}{HASH_SECRET}")));
    let mut h = reqwest::header::HeaderMap::new();
    for (k, v) in [
        ("user-agent", UA),
        ("app-os", "android"),
        ("app-os-version", "11"),
        ("x-client-time", &time),
        ("x-client-hash", &hash),
    ] {
        h.insert(
            reqwest::header::HeaderName::from_static(k),
            v.parse().unwrap(),
        );
    }
    h
}
fn form(fields: &[(&str, &str)]) -> Zeroizing<String> {
    let mut pairs = url::form_urlencoded::Serializer::new(String::new());
    pairs.extend_pairs([
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("include_policy", "true"),
    ]);
    pairs.extend_pairs(fields.iter().copied());
    Zeroizing::new(pairs.finish())
}
fn parse(status: reqwest::StatusCode, bytes: &[u8]) -> Result<TokenSet, TokenError> {
    if status.is_server_error() {
        return Err(TokenError::Unavailable);
    }
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        #[serde(default)]
        has_error: bool,
    }
    let envelope: ErrorEnvelope = serde_json::from_slice(bytes).map_err(|_| TokenError::Other)?;
    if envelope.has_error {
        return Err(TokenError::Rejected);
    }
    if !status.is_success() {
        let mut v: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| TokenError::Other)?;
        let rejected = status.as_u16() == 400
            && (v["error"] == "invalid_grant"
                || v["has_error"] == true
                || v["errors"]["system"]["code"] == 1508);
        crate::broker::wipe(&mut v);
        return Err(if rejected {
            TokenError::Rejected
        } else {
            TokenError::Other
        });
    }
    #[derive(Deserialize)]
    struct Reply {
        access_token: Secret,
        refresh_token: Secret,
        expires_in: u64,
        user: serde_json::Value,
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| TokenError::Other)?;
    let u = &reply.user;
    let id = match &u["id"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return Err(TokenError::Other),
    };
    Ok(TokenSet {
        access: reply.access_token,
        refresh: reply.refresh_token,
        expires_at: Instant::now() + Duration::from_secs(reply.expires_in.min(86400 * 365)),
        user: PixivUser {
            id,
            name: u["name"].as_str().unwrap_or_default().into(),
            account: u["account"].as_str().unwrap_or_default().into(),
            premium: u["is_premium"].as_bool().unwrap_or(false),
            x_restrict: u["x_restrict"]
                .as_i64()
                .or_else(|| u["x_restrict"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0),
        },
    })
}
impl Config {
    pub async fn exchange(&self, code: &str, verifier: &Secret) -> Result<TokenSet, TokenError> {
        let body = form(&[
            ("code", code),
            ("code_verifier", verifier.expose()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", REDIRECT_URI),
        ]);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| TokenError::Other)?;
        let r = client
            .post(&self.token_url)
            .headers(headers())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body.to_string())
            .send()
            .await
            .map_err(|_| TokenError::Unavailable)?;
        let status = r.status();
        if status.is_server_error() {
            return Err(TokenError::Unavailable);
        }
        let bytes = Zeroizing::new(
            r.bytes()
                .await
                .map_err(|_| TokenError::Unavailable)?
                .to_vec(),
        );
        parse(status, &bytes)
    }
    /// Must be called from a blocking worker, never a Tokio runtime worker.
    pub fn refresh(&self, token: &Secret) -> Result<TokenSet, TokenError> {
        let body = form(&[
            ("refresh_token", token.expose()),
            ("grant_type", "refresh_token"),
            ("get_secure_url", "1"),
        ]);
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| TokenError::Other)?;
        let r = client
            .post(&self.token_url)
            .headers(headers())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body.to_string())
            .send()
            .map_err(|_| TokenError::Unavailable)?;
        let status = r.status();
        if status.is_server_error() {
            return Err(TokenError::Unavailable);
        }
        let bytes = Zeroizing::new(r.bytes().map_err(|_| TokenError::Unavailable)?.to_vec());
        parse(status, &bytes)
    }
}
