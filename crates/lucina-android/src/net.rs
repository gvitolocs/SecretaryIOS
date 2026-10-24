//! One protocol-2 `POST /incoming` to the relay.
//!
//! rustls with a client-only graviola provider ([`crate::tls`]) and the
//! webpki roots, HTTPS only, to
//! [`ORIGIN`] only (the base URL is fixed in production; tests build a
//! plain-HTTP transport with `#[cfg(test)]` constructors). No redirects,
//! connect timeout 3 s, total 5 s, responses at most 4 KiB. One agent per
//! process, created on first use, so the TLS connection is reused.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lucina_phone_core::proto::{Incoming, IncomingAck, ORIGIN};
use ureq::tls::{TlsConfig, TlsProvider};
use ureq::Agent;
use zeroize::{Zeroize, Zeroizing};

/// Responses longer than this are refused.
pub const MAX_RESPONSE_BYTES: u64 = 4096;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);
const USER_AGENT: &str = concat!("lucina-otp/", env!("CARGO_PKG_VERSION"));

/// What the relay did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 200 with `accepted: true` (a repeat or a test event counts too).
    Accepted { test: bool },
    /// 409: forwarding is paused. Drop the code.
    Paused,
    /// 401: the token is not the phone's. Drop the code.
    Unpaired,
    /// 410: the code was already expired. Drop it.
    Expired,
    /// 400, any other 4xx, a redirect or `accepted: false`. Drop it.
    Rejected,
    /// Network error, timeout, 408, 429 or 5xx. Queue it.
    Retry,
}

/// Maps a relay response to an [`Outcome`]. `body` is `None` when it could
/// not be read within [`MAX_RESPONSE_BYTES`].
pub fn classify(status: u16, body: Option<&[u8]>) -> Outcome {
    match status {
        200 => match body.map(serde_json::from_slice::<IncomingAck>) {
            Some(Ok(ack)) if ack.accepted => Outcome::Accepted { test: ack.test },
            Some(_) => Outcome::Rejected,
            // The relay may have stored it; a resend with the same event id is a duplicate.
            None => Outcome::Retry,
        },
        409 => Outcome::Paused,
        401 => Outcome::Unpaired,
        410 => Outcome::Expired,
        408 | 429 | 500..=599 => Outcome::Retry,
        _ => Outcome::Rejected,
    }
}

/// The relay endpoint and its (lazily built) HTTP agent.
pub struct Transport {
    base: String,
    https_only: bool,
    agent: OnceLock<Agent>,
}

impl Default for Transport {
    fn default() -> Self {
        Self::production()
    }
}

impl Transport {
    /// HTTPS to [`ORIGIN`]; the only constructor outside tests.
    pub fn production() -> Self {
        Self {
            base: ORIGIN.to_owned(),
            https_only: true,
            agent: OnceLock::new(),
        }
    }

    /// Plain HTTP to a mock relay at `base` (tests only).
    #[cfg(test)]
    pub(crate) fn plain_http_for_tests(base: String) -> Self {
        Self {
            base,
            https_only: false,
            agent: OnceLock::new(),
        }
    }

    /// The production agent configuration pointed at `base` (tests only), to
    /// check that plain HTTP is refused.
    #[cfg(test)]
    pub(crate) fn https_only_for_tests(base: String) -> Self {
        Self {
            base,
            https_only: true,
            agent: OnceLock::new(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub(crate) fn agent(&self) -> &Agent {
        self.agent.get_or_init(|| {
            let tls = TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .unversioned_rustls_crypto_provider(Arc::new(crate::tls::provider()))
                .build();
            Agent::config_builder()
                .tls_config(tls)
                .https_only(self.https_only)
                .http_status_as_error(false)
                .max_redirects(0)
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_global(Some(TOTAL_TIMEOUT))
                .user_agent(USER_AGENT)
                .max_idle_connections(1)
                .max_idle_connections_per_host(1)
                .build()
                .new_agent()
        })
    }

    /// Sends `event` with the phone `token`. Returns the outcome and the
    /// round-trip time in ms.
    pub fn post_incoming(&self, token: &str, event: &Incoming) -> (Outcome, u64) {
        let start = Instant::now();
        let outcome = self.send(token, event);
        (outcome, elapsed_ms(start))
    }

    fn send(&self, token: &str, event: &Incoming) -> Outcome {
        let Ok(body) = serde_json::to_vec(event).map(Zeroizing::new) else {
            return Outcome::Rejected;
        };
        let auth = Zeroizing::new(format!("Bearer {token}"));
        let url = format!("{}/incoming", self.base);
        let response = self
            .agent()
            .post(&url)
            .header("authorization", auth.as_str())
            .content_type("application/json")
            .send(&body[..]);
        let mut response = match response {
            Ok(r) => r,
            Err(_) => return Outcome::Retry,
        };
        let status = response.status().as_u16();
        // Reading the body also lets the agent reuse the connection.
        let read = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec();
        let outcome = classify(status, read.as_deref().ok());
        if let Ok(mut b) = read {
            b.zeroize();
        }
        outcome
    }
}

pub(crate) fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}
