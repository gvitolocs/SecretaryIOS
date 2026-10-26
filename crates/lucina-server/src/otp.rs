//! The owner asking for an OTP (`/otp`, "what's my OTP", `/otp arm|stop|status`),
//! answered before the model (Node `secretary-api.js` `handleChat`,
//! `private-otp.js`, `tools/lucina-device` `otp::chat`).
//!
//! - The request never reaches the model, the timeline, the recent turns,
//!   Honcho or the `clientMessageId` replay cache: [`secretary_chat`] runs
//!   first in `chat::handle_chat` and returns the whole reply.
//! - Codes go only to the owner's own private chat
//!   ([`lucina_phone_core::reply::private_owner`]): Lucina MCP turns get
//!   [`reply::NOT_PRIVATE`].
//! - The relay (`docs/lucina-android.md`, "Relay protocol") is called with
//!   the owner token from `LUCINA_OTP_OWNER_TOKEN_FILE`, read per call
//!   (unset or unreadable: [`reply::UNAVAILABLE`]); 5 s timeout, no
//!   redirects, responses of at most 8 KiB.
//! - Replies are `sensitive: true`; a delivered code carries `expiresAt` so
//!   the Secretary client drops it then and never caches it.
//!
//! Telegram: `crate::telegram` (Node's `privateOtpMiddleware`) calls
//! [`respond`] with its [`Asker`] before the allow-list and any turn.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use lucina_phone_core::proto::{self, Delivered, RelayStatus, WindowState};
use lucina_phone_core::reply::{self, Asker};
use lucina_phone_core::{otp_intent, OtpAction};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// Whole-request timeout for one relay call.
pub const RELAY_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 8192;
/// `/otp arm` and the arm after a 204: the relay's longest window.
const ARM_SECONDS: u64 = 300;

/// `LUCINA_OTP_OWNER_TOKEN_FILE`.
#[derive(Debug, Clone)]
pub struct OtpConfig {
    /// The relay's owner token (64 hex, mode 0600), read per call. Unset:
    /// OTP requests answer [`reply::UNAVAILABLE`].
    pub owner_token_file: Option<PathBuf>,
    /// [`proto::ORIGIN`]. Not configurable from the environment, so the
    /// owner token only ever goes to the real relay; tests point it at a
    /// mock.
    pub relay_url: String,
}

impl Default for OtpConfig {
    fn default() -> Self {
        Self {
            owner_token_file: None,
            relay_url: proto::ORIGIN.to_owned(),
        }
    }
}

impl OtpConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        Self {
            owner_token_file: get("LUCINA_OTP_OWNER_TOKEN_FILE")
                .map(|p| PathBuf::from(p.trim()))
                .filter(|p| !p.as_os_str().is_empty()),
            ..Self::default()
        }
    }
}

/// An OTP answer. `expires_at` (Unix ms) only when it carries a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpReply {
    pub reply: String,
    pub expires_at: Option<u64>,
}

impl OtpReply {
    fn text(reply: &str) -> Self {
        Self {
            reply: reply.to_owned(),
            expires_at: None,
        }
    }
}

/// Why the relay could not answer. Never carries a token or a code.
#[derive(Debug)]
struct Unavailable(&'static str);

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(RELAY_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("OTP relay client")
    })
}

/// A plausible bearer token: what a pairing writes (64 hex), loosely.
fn usable_token(t: &str) -> bool {
    (32..=256).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

struct Relay<'a> {
    cfg: &'a OtpConfig,
}

impl Relay<'_> {
    async fn token(&self) -> Result<String, Unavailable> {
        let path = self
            .cfg
            .owner_token_file
            .as_ref()
            .ok_or(Unavailable("not configured"))?;
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|_| Unavailable("token file unreadable"))?;
        let token = raw.trim().to_owned();
        if !usable_token(&token) {
            return Err(Unavailable("token file invalid"));
        }
        // Every sink masks it, should it ever be echoed.
        lucina_secrets::redactor().register("LUCINA_OTP_OWNER_TOKEN", &token);
        Ok(token)
    }

    /// One owner call. `Ok(None)`: 204 (nothing waiting).
    async fn call(&self, route: &str, arm: Option<u64>) -> Result<Option<Vec<u8>>, Unavailable> {
        let token = self.token().await?;
        let url = format!("{}/{route}", self.cfg.relay_url.trim_end_matches('/'));
        let req = match arm {
            Some(seconds) => client().post(&url).json(&proto::ArmRequest {
                seconds: seconds.clamp(30, 300),
            }),
            None => client().get(&url),
        };
        let mut resp = req
            .bearer_auth(&token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| Unavailable("relay unreachable"))?;
        drop(token);
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Unavailable("relay refused"));
        }
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Unavailable("relay response too large"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| Unavailable("relay response cut"))?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(Unavailable("relay response too large"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }

    async fn json<T: DeserializeOwned>(
        &self,
        route: &str,
        arm: Option<u64>,
    ) -> Result<T, Unavailable> {
        let body = self
            .call(route, arm)
            .await?
            .ok_or(Unavailable("relay answered 204"))?;
        serde_json::from_slice(&body).map_err(|_| Unavailable("relay response invalid"))
    }

    async fn status(&self) -> Result<RelayStatus, Unavailable> {
        self.json("status", None).await
    }

    async fn arm(&self) -> Result<WindowState, Unavailable> {
        self.json("arm", Some(ARM_SECONDS)).await
    }

    async fn disarm(&self) -> Result<(), Unavailable> {
        // The relay ignores the body; the 0.4 relay expected one.
        self.call("disarm", Some(30)).await.map(|_| ())
    }

    async fn take(&self) -> Result<Option<Delivered>, Unavailable> {
        match self.call("otp", None).await? {
            None => Ok(None),
            Some(body) => serde_json::from_slice(&body)
                .map(Some)
                .map_err(|_| Unavailable("relay response invalid")),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

async fn run(relay: &Relay<'_>, action: OtpAction) -> Result<OtpReply, Unavailable> {
    Ok(match action {
        OtpAction::Disarm => {
            relay.disarm().await?;
            OtpReply::text(reply::DISARMED)
        }
        OtpAction::Status => OtpReply::text(reply::status(relay.status().await?.armed)),
        OtpAction::Arm => OtpReply::text(reply::armed(relay.arm().await?.continuous)),
        OtpAction::Take => match relay.take().await? {
            Some(d) => {
                if !proto::valid_code(&d.code) {
                    return Err(Unavailable("invalid code"));
                }
                if d.expires_at <= now_ms() {
                    return Err(Unavailable("expired code"));
                }
                OtpReply {
                    reply: reply::delivered(&d),
                    expires_at: Some(d.expires_at),
                }
            }
            None => {
                let status = relay.status().await?;
                let continuous = if status.armed {
                    status.continuous
                } else {
                    relay.arm().await?.continuous
                };
                OtpReply::text(reply::waiting(continuous))
            }
        },
    })
}

/// The answer to `action` for `asker`: [`reply::NOT_PRIVATE`] unless it is
/// the owner's own private chat; [`reply::UNAVAILABLE`] when the relay (or
/// its token) is not there.
pub async fn respond(cfg: &OtpConfig, asker: &Asker<'_>, action: OtpAction) -> OtpReply {
    if !reply::private_owner(asker) {
        tracing::warn!(
            surface = asker.surface,
            mcp = asker.mcp,
            action = action.as_str(),
            "otp request refused: not the owner's private chat"
        );
        return OtpReply::text(reply::NOT_PRIVATE);
    }
    match run(&Relay { cfg }, action).await {
        Ok(r) => {
            tracing::info!(
                action = action.as_str(),
                delivered = r.expires_at.is_some(),
                "otp request answered"
            );
            r
        }
        Err(Unavailable(why)) => {
            tracing::warn!(action = action.as_str(), why, "otp relay unavailable");
            OtpReply::text(reply::UNAVAILABLE)
        }
    }
}

/// `/api/secretary/chat`: `None` when `text` (the message without the MCP
/// prefix) is not an OTP request; otherwise the whole reply, in the Node
/// shape. `mcp`: the turn came through Lucina MCP.
pub async fn secretary_chat(
    cfg: &OtpConfig,
    text: &str,
    mcp: bool,
    session_id: &str,
    client_message_id: &str,
) -> Option<Value> {
    let action = otp_intent(text)?;
    let asker = Asker {
        surface: "app",
        owner_authorized: true,
        mcp,
        ..Asker::default()
    };
    if !reply::private_owner(&asker) {
        tracing::warn!(
            session_id,
            action = action.as_str(),
            "secretary mcp otp request refused"
        );
        return Some(json!({
            "reply": reply::NOT_PRIVATE,
            "action": "reply",
            "sessionId": session_id,
            "job": null,
        }));
    }
    let r = respond(cfg, &asker, action).await;
    let mut out = json!({
        "reply": r.reply,
        "action": "private_otp",
        "sensitive": true,
        "sessionId": session_id,
        "clientMessageId": (!client_message_id.is_empty()).then_some(client_message_id),
        "job": null,
        "userItemId": null,
        "lucinaItemId": null,
    });
    if let Some(at) = r.expires_at {
        out["expiresAt"] = json!(at);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_off_by_default() {
        let c = OtpConfig::from_lookup(&|_: &str| None);
        assert!(c.owner_token_file.is_none());
        assert_eq!(c.relay_url, proto::ORIGIN);
        let c = OtpConfig::from_lookup(&|k: &str| {
            (k == "LUCINA_OTP_OWNER_TOKEN_FILE").then(|| "/run/otp/owner.token".to_owned())
        });
        assert_eq!(
            c.owner_token_file.as_deref(),
            Some(std::path::Path::new("/run/otp/owner.token"))
        );
        assert_eq!(c.relay_url, proto::ORIGIN, "the relay URL has no variable");
    }

    #[test]
    fn token_shape() {
        assert!(usable_token(&"ab12".repeat(16)));
        assert!(!usable_token("short"));
        assert!(!usable_token(&format!("{}\nX: y", "a".repeat(40))));
    }

    #[tokio::test]
    async fn unconfigured_is_unavailable_and_mcp_is_refused() {
        let cfg = OtpConfig::default();
        let owner = Asker {
            surface: "app",
            owner_authorized: true,
            ..Asker::default()
        };
        assert_eq!(
            respond(&cfg, &owner, OtpAction::Take).await.reply,
            reply::UNAVAILABLE
        );
        let mcp = Asker { mcp: true, ..owner };
        assert_eq!(
            respond(&cfg, &mcp, OtpAction::Take).await.reply,
            reply::NOT_PRIVATE
        );
        assert!(secretary_chat(&cfg, "che tempo fa?", false, "s", "")
            .await
            .is_none());
    }
}
