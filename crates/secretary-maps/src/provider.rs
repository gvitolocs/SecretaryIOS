//! Google credentials and transport. Provider errors/bodies are deliberately never logged.
use crate::AppState;
use reqwest::header::HeaderValue;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub struct Unavailable;

#[derive(Clone)]
pub struct Secret(HeaderValue);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Secret {
    pub fn new(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }
        let mut h = HeaderValue::from_str(s).ok()?;
        h.set_sensitive(true);
        Some(Self(h))
    }
}

/// A Google client key (Maps JavaScript API on web, Maps SDK for iOS). Google restricts it to
/// the web referrer or the iOS bundle id; the apps receive it from `/v1/maps/config` and it is
/// never logged (`Debug` is redacted).
#[derive(Clone, PartialEq)]
pub struct ClientKey(String);
impl std::fmt::Debug for ClientKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl ClientKey {
    /// 20..=100 characters of `[A-Za-z0-9_-]` (the shape of a Google API key), else `None`.
    pub fn new(s: &str) -> Option<Self> {
        ((20..=100).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .then(|| Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub async fn post(
    st: &AppState,
    url: &str,
    mask: &str,
    body: &Value,
) -> Result<Value, Unavailable> {
    let key = st.config.google_key.as_ref().ok_or(Unavailable)?;
    // The outer deadline includes reading and parsing the response, not only headers.
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        let response = st
            .http
            .post(url)
            .header("X-Goog-Api-Key", key.0.clone())
            .header("X-Goog-FieldMask", mask)
            .json(body)
            .send()
            .await
            .map_err(|_| Unavailable)?;
        if !response.status().is_success() {
            return Err(Unavailable);
        }
        let response: Value = response.json().await.map_err(|_| Unavailable)?;
        if response.get("error").is_some() {
            return Err(Unavailable);
        }
        Ok(response)
    })
    .await
    .map_err(|_| Unavailable)?
}

/// Origin overrides get the standard API path; a supplied path is a complete endpoint.
pub(crate) fn endpoint(url: &str, path: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) if parsed.path() == "/" => {
            parsed.set_path(path);
            parsed.to_string()
        }
        _ => url.to_string(),
    }
}
