//! The pixiv core behind a small trait, so the service logic (gating, caching, spacing, backoff)
//! is testable without pixiv.net. The real implementation is `CoreBackend`, the blocking
//! `pixiv::api::PixivClient` with an empty proxy base (it then returns the original i.pximg.net
//! URLs; the service signs them itself). Calls are blocking: the caller runs them in
//! `spawn_blocking` behind the `ApiGate`.

use serde_json::Value;

pub trait Backend: Send + Sync + 'static {
    fn connection(&self) -> Option<&std::sync::Arc<crate::connection::Connection>> {
        None
    }
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String>;
    /// `args` are the core's CLI-style arguments: the words, then `--order X --mode X ...`.
    fn search(&self, args: &[String]) -> Result<Value, String>;
    fn illust(&self, id: &str) -> Result<Value, String>;
    /// The session's account flags (`user_status`: `is_logged_in`, `is_premium`, `x_restrict`).
    fn self_status(&self) -> Result<Value, String>;
    /// Whether a pixiv.net session is configured (no request).
    fn has_session(&self) -> bool;
}

/// Anonymous core adapter. The core's blocking client lives in a static (see `client`): a blocking
/// reqwest client owns a runtime and must never be dropped on an async worker.
pub struct CoreBackend;
impl CoreBackend {
    pub fn new(_session: Option<String>) -> Self {
        Self
    }
    /// Built on first use (always on a blocking worker) and kept for the process: its connections to
    /// pixiv.net are reused instead of a TLS handshake per call.
    fn client(&self) -> &'static pixiv::api::PixivClient {
        static CLIENT: std::sync::OnceLock<pixiv::api::PixivClient> = std::sync::OnceLock::new();
        CLIENT.get_or_init(|| pixiv::api::PixivClient::new(String::new()))
    }
}

impl Default for CoreBackend {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Backend for CoreBackend {
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String> {
        self.client().ranking(mode, page)
    }
    fn search(&self, args: &[String]) -> Result<Value, String> {
        self.client().search(args)
    }
    fn illust(&self, id: &str) -> Result<Value, String> {
        self.client().illust(id)
    }
    fn self_status(&self) -> Result<Value, String> {
        self.client().self_status()
    }
    fn has_session(&self) -> bool {
        false
    }
}

/// Anonymous web API or the connected App API, sharing exactly the HTTP connection.
pub struct SwitchBackend {
    anonymous: CoreBackend,
    app: crate::app_api::AppBackend,
    connection: std::sync::Arc<crate::connection::Connection>,
}
impl SwitchBackend {
    pub fn new(
        connection: std::sync::Arc<crate::connection::Connection>,
        app_base: String,
    ) -> Self {
        Self {
            anonymous: CoreBackend::new(None),
            app: crate::app_api::AppBackend::new(connection.clone(), app_base),
            connection,
        }
    }
    fn active(&self) -> &dyn Backend {
        if self.connection.connected() {
            &self.app
        } else {
            &self.anonymous
        }
    }
}
impl Backend for SwitchBackend {
    fn connection(&self) -> Option<&std::sync::Arc<crate::connection::Connection>> {
        Some(&self.connection)
    }
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String> {
        self.active().ranking(mode, page)
    }
    fn search(&self, args: &[String]) -> Result<Value, String> {
        self.active().search(args)
    }
    fn illust(&self, id: &str) -> Result<Value, String> {
        self.active().illust(id)
    }
    fn has_session(&self) -> bool {
        self.connection.has_session()
    }
    fn self_status(&self) -> Result<Value, String> {
        self.app.self_status()
    }
}

/// What a core error string means for the gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fail {
    /// HTTP 429: back off, then cool down.
    RateLimited,
    /// 5xx, network or read errors: retry with backoff.
    Transient,
    /// 404, or pixiv's own `{"error":true}` answer (a missing or hidden work).
    NotFound,
    /// A search with no results (the core reports it as an error).
    Empty,
    /// Anything else (403 from the CDN edge, bad JSON, ...): not retried.
    Other,
}

pub fn classify(e: &str) -> Fail {
    let e = e.trim();
    if let Some(rest) = e.strip_prefix("HTTP ") {
        let code: u16 = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        return match code {
            429 => Fail::RateLimited,
            404 | 410 => Fail::NotFound,
            500..=599 => Fail::Transient,
            _ => Fail::Other,
        };
    }
    if e.starts_with("network:") || e.starts_with("read:") {
        return Fail::Transient;
    }
    if e.starts_with("search: no illustManga") {
        return Fail::Empty;
    }
    if e.starts_with("pixiv:") {
        return Fail::NotFound;
    }
    Fail::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_core_errors() {
        assert_eq!(
            classify("HTTP 429 Too Many Requests: slow"),
            Fail::RateLimited
        );
        assert_eq!(classify("HTTP 404 Not Found: x"), Fail::NotFound);
        assert_eq!(classify("HTTP 503 Service Unavailable: x"), Fail::Transient);
        assert_eq!(classify("HTTP 403 Forbidden: x"), Fail::Other);
        assert_eq!(classify("network: timeout"), Fail::Transient);
        assert_eq!(classify("pixiv: 該当作品は削除されたか"), Fail::NotFound);
        assert_eq!(
            classify("search: no illustManga (word may have no results)"),
            Fail::Empty
        );
        assert_eq!(classify("bad JSON: eof"), Fail::Other);
    }
}
