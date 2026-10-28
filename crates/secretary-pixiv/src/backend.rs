//! The pixiv core behind a small trait, so the service logic (gating, caching, spacing, backoff)
//! is testable without pixiv.net. The real implementation is `CoreBackend`, the blocking
//! `pixiv::api::PixivClient` with an empty proxy base (it then returns the original i.pximg.net
//! URLs; the service signs them itself). Calls are blocking: the caller runs them in
//! `spawn_blocking` behind the `ApiGate`.

use serde_json::Value;

pub trait Backend: Send + Sync + 'static {
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String>;
    /// `args` are the core's CLI-style arguments: the words, then `--order X --mode X ...`.
    fn search(&self, args: &[String]) -> Result<Value, String>;
    fn illust(&self, id: &str) -> Result<Value, String>;
}

pub struct CoreBackend {
    client: pixiv::api::PixivClient,
}

impl CoreBackend {
    pub fn new() -> CoreBackend {
        CoreBackend {
            client: pixiv::api::PixivClient::new(String::new()),
        }
    }
}

impl Default for CoreBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for CoreBackend {
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String> {
        self.client.ranking(mode, page)
    }
    fn search(&self, args: &[String]) -> Result<Value, String> {
        self.client.search(args)
    }
    fn illust(&self, id: &str) -> Result<Value, String> {
        self.client.illust(id)
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
        assert_eq!(classify("HTTP 429 Too Many Requests: slow"), Fail::RateLimited);
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
