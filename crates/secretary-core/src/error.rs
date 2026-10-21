//! Failure classification for calls to Lucina.
//!
//! The variants are chosen so a caller (secretary-server) can map them to
//! HTTP without inspecting strings: `Api` carries Lucina's own status and
//! error code, transport failures are split into unreachable vs timeout.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Lucina answered with a non-2xx status (or a 2xx `{ok:false}` envelope,
    /// reported with status 502). `code` is Lucina's `error` field when the
    /// body was an envelope, otherwise `http_<status>`.
    #[error("lucina returned {status}: {code}")]
    Api {
        status: u16,
        code: String,
        message: Option<String>,
    },
    /// Connection refused / DNS / TLS / reset before a response arrived.
    #[error("lucina unreachable: {0}")]
    Unreachable(String),
    /// The per-request deadline elapsed.
    #[error("lucina timed out")]
    Timeout,
    /// Lucina answered 2xx but the body did not match the contract.
    #[error("unexpected response from lucina: {0}")]
    Decode(String),
    /// The caller's input failed validation before anything was sent.
    #[error("invalid {field}: {message}")]
    Invalid {
        field: &'static str,
        message: String,
    },
    /// Bad client construction (e.g. an unparsable base URL).
    #[error("invalid configuration: {0}")]
    Config(String),
}

impl Error {
    pub(crate) fn from_reqwest(e: reqwest::Error) -> Self {
        // Timeouts first: a connect timeout is also `is_connect()`, and it
        // must surface as a timeout.
        if e.is_timeout() {
            Error::Timeout
        } else if e.is_decode() {
            Error::Decode(e.to_string())
        } else {
            // Strip the URL so callers can log the error without leaking
            // query strings.
            Error::Unreachable(e.without_url().to_string())
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
