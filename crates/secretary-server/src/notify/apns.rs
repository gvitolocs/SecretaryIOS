//! APNs adapter for the native iOS app: **built, tested, and off.**
//!
//! Remote push to the native app needs a paid Apple Developer Program team
//! (the free Personal Team cannot use the `aps-environment` entitlement)
//! and an APNs auth key (`AuthKey_<KEYID>.p8`). Until those exist the
//! adapter stays disabled: `POST /v1/push/apns` answers `503 apns_disabled`
//! and nothing is ever sent to Apple.
//!
//! Turn it on by setting all of:
//!
//! | variable | |
//! |---|---|
//! | `SECRETARY_APNS_KEY_FILE` | the `.p8` key (PKCS#8 PEM), mode 0600 |
//! | `SECRETARY_APNS_KEY_ID` | its 10-character key id |
//! | `SECRETARY_APNS_TEAM_ID` | the 10-character team id |
//! | `SECRETARY_APNS_TOPIC` | bundle id, default `tech.getsecretary.app` |
//! | `SECRETARY_APNS_ENV` | `sandbox` (Xcode builds, default) or `production` (TestFlight/App Store) |
//!
//! Requests go over HTTP/2 to `api.push.apple.com` (or the sandbox host)
//! with a provider JWT (ES256, `kid` = key id, `iss` = team id, `iat`),
//! refreshed every 50 minutes as Apple requires (20–60 min).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde_json::json;

use super::push::{apns_body, Payload, PushOpts};

pub const DEFAULT_TOPIC: &str = "tech.getsecretary.app";
pub const PRODUCTION_URL: &str = "https://api.push.apple.com";
pub const SANDBOX_URL: &str = "https://api.sandbox.push.apple.com";
const TOKEN_REFRESH: Duration = Duration::from_secs(50 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApnsConfig {
    pub key_file: PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
    pub sandbox: bool,
    /// Override of the APNs origin (tests).
    pub base_url: Option<String>,
}

impl ApnsConfig {
    /// `None` when no `SECRETARY_APNS_*` key/team variable is set; an error
    /// when only some are.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let key_file = get("SECRETARY_APNS_KEY_FILE");
        let key_id = get("SECRETARY_APNS_KEY_ID");
        let team_id = get("SECRETARY_APNS_TEAM_ID");
        let (key_file, key_id, team_id) = match (key_file, key_id, team_id) {
            (None, None, None) => return Ok(None),
            (Some(f), Some(k), Some(t)) => (f, k, t),
            _ => bail!(
                "APNs needs SECRETARY_APNS_KEY_FILE, SECRETARY_APNS_KEY_ID and SECRETARY_APNS_TEAM_ID together"
            ),
        };
        let id_ok = |s: &str| s.len() == 10 && s.bytes().all(|b| b.is_ascii_alphanumeric());
        if !id_ok(&key_id) || !id_ok(&team_id) {
            bail!("SECRETARY_APNS_KEY_ID and SECRETARY_APNS_TEAM_ID are 10 letters/digits");
        }
        let sandbox = match get("SECRETARY_APNS_ENV").as_deref() {
            None | Some("sandbox") | Some("development") => true,
            Some("production") => false,
            Some(v) => bail!("SECRETARY_APNS_ENV={v}: expected sandbox or production"),
        };
        Ok(Some(Self {
            key_file: PathBuf::from(key_file),
            key_id,
            team_id,
            topic: get("SECRETARY_APNS_TOPIC").unwrap_or_else(|| DEFAULT_TOPIC.into()),
            sandbox,
            base_url: None,
        }))
    }

    pub fn origin(&self) -> &str {
        self.base_url.as_deref().unwrap_or(if self.sandbox {
            SANDBOX_URL
        } else {
            PRODUCTION_URL
        })
    }
}

pub struct Apns {
    pub cfg: ApnsConfig,
    key: SigningKey,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

/// An APNs device token: hex, 64–200 characters.
pub fn valid_device_token(t: &str) -> bool {
    (64..=200).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Apns {
    pub fn new(cfg: ApnsConfig) -> anyhow::Result<Self> {
        let pem = std::fs::read_to_string(&cfg.key_file)
            .with_context(|| format!("SECRETARY_APNS_KEY_FILE {}", cfg.key_file.display()))?;
        let key = SigningKey::from_pkcs8_pem(&pem)
            .map_err(|_| anyhow::anyhow!("SECRETARY_APNS_KEY_FILE is not a P-256 PKCS#8 key"))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            cfg,
            key,
            http,
            token: Mutex::new(None),
        })
    }

    /// The provider JWT for `iat` (seconds).
    pub fn jwt_at(&self, iat: u64) -> String {
        let header = json!({"alg": "ES256", "kid": self.cfg.key_id});
        let claims = json!({"iss": self.cfg.team_id, "iat": iat});
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig: Signature = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }

    fn jwt(&self) -> String {
        let mut slot = self.token.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, at)) = slot.as_ref() {
            if at.elapsed() < TOKEN_REFRESH {
                return t.clone();
            }
        }
        let iat = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let t = self.jwt_at(iat);
        *slot = Some((t.clone(), Instant::now()));
        t
    }

    /// One POST to APNs: `(status, Retry-After, reason)`; a dead token's
    /// 400 reason is reported as 410 so the caller drops it.
    pub async fn send(
        &self,
        device_token: &str,
        payload: &Payload,
        opts: &PushOpts,
    ) -> Result<(u16, Option<Duration>, String), String> {
        if !valid_device_token(device_token) {
            return Ok((410, None, "BadDeviceToken".into()));
        }
        let url = format!("{}/3/device/{device_token}", self.cfg.origin());
        let expiration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 6 * 3600;
        let mut req = self
            .http
            .post(url)
            .header("authorization", format!("bearer {}", self.jwt()))
            .header("apns-topic", &self.cfg.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", if opts.urgent { "10" } else { "5" })
            .header("apns-expiration", expiration.to_string())
            .json(&apns_body(payload));
        if let Some(t) = &opts.topic {
            req = req.header("apns-collapse-id", t);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                "timeout".to_owned()
            } else {
                "network error".to_owned()
            }
        })?;
        let status = resp.status().as_u16();
        let retry = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()?.trim().parse().ok())
            .map(Duration::from_secs);
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["reason"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let dead = matches!(
            reason.as_str(),
            "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic"
        );
        Ok((if dead { 410 } else { status }, retry, reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;
    use p256::pkcs8::{EncodePrivateKey, LineEnding};

    fn key_file(dir: &std::path::Path) -> (PathBuf, VerifyingKey) {
        let sk = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let pem = sk.to_pkcs8_pem(LineEnding::LF).unwrap();
        let path = dir.join("AuthKey_TEST123456.p8");
        std::fs::write(&path, pem.as_bytes()).unwrap();
        (path, *sk.verifying_key())
    }

    #[test]
    fn config_needs_all_three_or_none() {
        let none = ApnsConfig::from_lookup(&|_: &str| None).unwrap();
        assert!(none.is_none(), "disabled by default");
        let partial = ApnsConfig::from_lookup(&|k: &str| {
            (k == "SECRETARY_APNS_KEY_ID").then(|| "ABCDE12345".to_owned())
        });
        assert!(partial.is_err());
        let full = ApnsConfig::from_lookup(&|k: &str| match k {
            "SECRETARY_APNS_KEY_FILE" => Some("/x.p8".into()),
            "SECRETARY_APNS_KEY_ID" => Some("ABCDE12345".into()),
            "SECRETARY_APNS_TEAM_ID" => Some("TEAM123456".into()),
            "SECRETARY_APNS_ENV" => Some("production".into()),
            _ => None,
        })
        .unwrap()
        .unwrap();
        assert_eq!(full.topic, DEFAULT_TOPIC);
        assert_eq!(full.origin(), PRODUCTION_URL);
    }

    #[test]
    fn provider_jwt_is_es256_with_kid_iss_iat() {
        let dir = tempfile::tempdir().unwrap();
        let (path, vk) = key_file(dir.path());
        let apns = Apns::new(ApnsConfig {
            key_file: path,
            key_id: "TEST123456".into(),
            team_id: "TEAM123456".into(),
            topic: DEFAULT_TOPIC.into(),
            sandbox: true,
            base_url: None,
        })
        .unwrap();
        let jwt = apns.jwt_at(1_761_048_000);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(header, json!({"alg": "ES256", "kid": "TEST123456"}));
        assert_eq!(claims, json!({"iss": "TEAM123456", "iat": 1_761_048_000}));
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
        assert_eq!(apns.jwt(), apns.jwt(), "cached between calls");
        assert!(valid_device_token(&"ab".repeat(32)));
        assert!(!valid_device_token("xyz"));
    }
}
