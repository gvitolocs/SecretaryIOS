//! The OTP relay protocol (`docs/lucina-android.md`, "Relay protocol").
//!
//! The relay listens on `127.0.0.1:18866` behind Caddy at
//! [`ORIGIN`] (`https://getsecretary.tech/v1/phone/otp/*`).
//!
//! | Route | Token | Body → response |
//! |---|---|---|
//! | `GET /window` | phone | → [`WindowState`] (protocol 1 only) |
//! | `POST /incoming` | phone | [`Incoming`] → [`IncomingAck`]; 409 when paused or the window is wrong |
//! | `GET /status` | owner | → [`RelayStatus`] |
//! | `POST /arm`, `POST /disarm` | owner | [`ArmRequest`] → [`WindowState`] / `{disarmed:true}` |
//! | `GET /otp` | owner | → [`Delivered`], or 204 when nothing is waiting |
//!
//! Protocol 1 (app 0.4.0) fetches `/window` and sends its `windowId` with
//! every code. Protocol 2 (app 1.x) sends one `POST /incoming` without
//! `windowId`; the relay accepts it while forwarding is armed. `test: true`
//! events are validated and acknowledged but never stored, so the phone can
//! measure the whole path without a fake code reaching the owner.

use serde::{Deserialize, Serialize};

/// Public base URL of the relay.
pub const ORIGIN: &str = "https://getsecretary.tech/v1/phone/otp";
/// Newest protocol version.
pub const PROTOCOL_VERSION: u32 = 2;
/// How long a code stays deliverable after it was received.
pub const CODE_TTL_MS: u64 = 120_000;
/// Request and response bodies are at most this long.
pub const MAX_BODY_BYTES: usize = 2048;
/// Sender names are cut to this many characters.
pub const MAX_SENDER_CHARS: usize = 80;

/// Where the phone saw the code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OtpSource {
    #[default]
    Sms,
    Notification,
}

/// `POST /incoming`: one code seen on the phone. Never carries the message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    /// Protocol 1 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// Random id (UUID) per received message; the relay drops repeats.
    pub event_id: String,
    pub code: String,
    #[serde(default = "default_sender")]
    pub sender: String,
    /// Unix ms when the phone received the message (protocol 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<u64>,
    #[serde(default)]
    pub source: OtpSource,
    /// Validate and acknowledge only; never stored or delivered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

fn default_sender() -> String {
    "SMS".into()
}

/// Why a request body was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoError {
    InvalidCode,
    InvalidEventId,
    InvalidWindow,
    Expired,
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidCode => "invalid code",
            Self::InvalidEventId => "invalid event id",
            Self::InvalidWindow => "invalid window",
            Self::Expired => "expired",
        })
    }
}

impl std::error::Error for ProtoError {}

impl Incoming {
    /// Shape checks shared by the phone (before sending) and the relay.
    pub fn validate(&self) -> Result<(), ProtoError> {
        if !valid_code(&self.code) {
            return Err(ProtoError::InvalidCode);
        }
        if !valid_event_id(&self.event_id) {
            return Err(ProtoError::InvalidEventId);
        }
        if self
            .window_id
            .as_deref()
            .is_some_and(|w| !valid_window_id(w))
        {
            return Err(ProtoError::InvalidWindow);
        }
        Ok(())
    }
}

/// `POST /incoming` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingAck {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub duplicate: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

/// `GET /window` and `POST /arm`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub armed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// Unix ms; `None` while continuous.
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub continuous: bool,
}

/// `GET /otp`: the newest fresh code, removed from the relay by this read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivered {
    pub sender: String,
    pub code: String,
    pub received_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub source: OtpSource,
}

/// `GET /status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayStatus {
    pub version: String,
    pub transport: String,
    pub armed: bool,
    #[serde(default)]
    pub continuous: bool,
    /// Newest protocol the relay speaks (absent = 1).
    #[serde(default = "protocol_one")]
    pub protocol: u32,
}

fn protocol_one() -> u32 {
    1
}

/// `POST /arm`, `POST /disarm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmRequest {
    /// 30–300 s; ignored while the relay runs continuous.
    pub seconds: u64,
}

/// 4–10 ASCII alphanumerics.
pub fn valid_code(code: &str) -> bool {
    (4..=10).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// 16–80 ASCII alphanumerics or `-` (a UUID fits).
pub fn valid_event_id(id: &str) -> bool {
    (16..=80).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// 16–96 lower-case hex characters.
pub fn valid_window_id(id: &str) -> bool {
    (16..=96).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A pairing token: 64 lower-case hex characters.
pub fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A sender name safe to show: no control characters or angle brackets,
/// at most [`MAX_SENDER_CHARS`] characters, `SMS` when empty.
pub fn clean_sender(sender: &str) -> String {
    let s: String = sender
        .chars()
        .filter(|c| !c.is_control() && *c != '<' && *c != '>')
        .take(MAX_SENDER_CHARS)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        default_sender()
    } else {
        s.to_owned()
    }
}

/// When a code received at `received_at` (phone clock, optional) stops being
/// deliverable, given the relay's clock `now` and the window's end.
///
/// A phone timestamp in the future counts as `now`; one older than
/// [`CODE_TTL_MS`] means the code is already expired.
pub fn expires_at(
    received_at: Option<u64>,
    now: u64,
    window_expires: u64,
) -> Result<u64, ProtoError> {
    let base = received_at.map_or(now, |t| t.min(now));
    let expires = base.saturating_add(CODE_TTL_MS).min(window_expires);
    if expires <= now {
        Err(ProtoError::Expired)
    } else {
        Ok(expires)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_one_body_still_parses() {
        let v1: Incoming = serde_json::from_value(json!({
            "windowId": "0123456789abcdef0123", "eventId": "0123456789abcdef", "code": "482917", "sender": "Bank"
        }))
        .unwrap();
        assert_eq!(v1.source, OtpSource::Sms);
        assert!(v1.validate().is_ok());
        assert_eq!(
            serde_json::to_value(IncomingAck {
                accepted: true,
                ..Default::default()
            })
            .unwrap(),
            json!({"accepted": true})
        );
    }

    #[test]
    fn validation_and_sender() {
        let mut e = Incoming {
            window_id: None,
            event_id: "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e11".into(),
            code: "X8H2K9".into(),
            sender: "Bank".into(),
            received_at: Some(1),
            source: OtpSource::Notification,
            test: false,
        };
        assert!(e.validate().is_ok());
        e.code = "12".into();
        assert_eq!(e.validate(), Err(ProtoError::InvalidCode));
        assert_eq!(clean_sender("<b>Bank\u{7}</b>"), "bBank/b");
        assert_eq!(clean_sender("  "), "SMS");
        assert!(valid_token(&"a".repeat(64)) && !valid_token(&"A".repeat(64)));
    }

    #[test]
    fn expiry() {
        let now = 1_000_000;
        assert_eq!(expires_at(None, now, u64::MAX), Ok(now + CODE_TTL_MS));
        assert_eq!(
            expires_at(Some(now - 20_000), now, u64::MAX),
            Ok(now + 100_000)
        );
        assert_eq!(
            expires_at(Some(now + 50_000), now, u64::MAX),
            Ok(now + CODE_TTL_MS)
        );
        assert_eq!(
            expires_at(Some(now - CODE_TTL_MS), now, u64::MAX),
            Err(ProtoError::Expired)
        );
        assert_eq!(expires_at(None, now, now + 5), Ok(now + 5));
    }
}
