//! Grant verification (contract v1).
//!
//! `grant = b64url(payload_json) + "." + b64url(sig)`; the Ed25519 signature
//! covers the ASCII bytes `"secretary-approval-v1." + b64url(payload_json)`.
//! A verifier checks the signature with the pinned public key, `exp`, the
//! action, that `payloadHash` equals the hash of what it is about to do, and
//! (separately, see [`crate::used`]) single use.

use std::path::Path;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;

/// Prefix of the signed message.
pub const SIGN_PREFIX: &str = "secretary-approval-v1.";

/// Why a grant was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    #[error("the grant is malformed ({0})")]
    Malformed(&'static str),
    #[error("the grant signature does not verify with the pinned public key")]
    BadSignature,
    #[error("the grant is not an approval")]
    NotApproved,
    #[error("the grant has expired")]
    Expired,
    #[error("the grant is for another action ({got}, expected {want})")]
    WrongAction { got: String, want: String },
    #[error("the grant is for different content than what would run")]
    WrongPayload,
    #[error("the grant belongs to another approval")]
    WrongId,
    #[error("the grant has no biometric proof")]
    NotBiometric,
    #[error("the grant was already used")]
    Replay,
}

/// The pinned Ed25519 public key (32 bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct PubKey([u8; 32]);

impl std::fmt::Debug for PubKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PubKey(ed25519)")
    }
}

/// DER prefix of an Ed25519 SubjectPublicKeyInfo.
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

fn b64_any(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|e| e.decode(s).ok())
}

impl PubKey {
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The server's `GET /v1/approvals/public-key` JSON
    /// (`{"alg":"ed25519","publicKey":"<b64>","keyId":"…"}`) or the raw
    /// base64 key (also an SPKI DER in base64).
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim();
        let b64: String = if t.starts_with('{') {
            let v: serde_json::Value =
                serde_json::from_str(t).map_err(|e| format!("public key JSON: {e}"))?;
            if let Some(alg) = v["alg"].as_str() {
                if !alg.eq_ignore_ascii_case("ed25519") {
                    return Err(format!("public key algorithm {alg} is not ed25519"));
                }
            }
            v["publicKey"]
                .as_str()
                .ok_or("public key JSON has no publicKey")?
                .to_owned()
        } else {
            t.to_owned()
        };
        let raw = b64_any(&b64).ok_or("public key is not base64")?;
        let key: &[u8] = if raw.len() == 44 && raw.starts_with(&SPKI_PREFIX) {
            &raw[12..]
        } else {
            &raw
        };
        let arr: [u8; 32] = key
            .try_into()
            .map_err(|_| format!("public key is {} bytes, expected 32", key.len()))?;
        Ok(Self(arr))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("public key file {}: {}", path.display(), e.kind()))?;
        Self::parse(&text)
    }
}

/// The requester recorded in a grant.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct GrantRequester {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
}

/// A verified grant's payload.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Claims {
    pub v: u32,
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub requester: GrantRequester,
    #[serde(rename = "payloadHash")]
    pub payload_hash: String,
    pub decision: String,
    #[serde(rename = "decidedBy", default)]
    pub decided_by: String,
    #[serde(default)]
    pub biometric: bool,
    #[serde(default)]
    pub iat: i64,
    pub exp: i64,
}

/// What the verifier is about to do.
#[derive(Debug, Clone)]
pub struct Expect<'a> {
    pub action: &'a str,
    pub payload_hash: &'a str,
    /// The approval id this grant must belong to (when known).
    pub id: Option<&'a str>,
    pub require_biometric: bool,
    /// Unix seconds.
    pub now: i64,
}

/// Check signature, expiry, action, payload hash (and id, biometric).
/// Single use is checked by the caller with [`crate::used::UsedRegistry`].
pub fn verify(key: &PubKey, grant: &str, expect: &Expect<'_>) -> Result<Claims, GrantError> {
    let (payload_b64, sig_b64) = grant
        .trim()
        .split_once('.')
        .ok_or(GrantError::Malformed("no separator"))?;
    if sig_b64.contains('.') {
        return Err(GrantError::Malformed("too many parts"));
    }
    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|_| GrantError::Malformed("signature is not base64url"))?;
    let message = format!("{SIGN_PREFIX}{payload_b64}");
    UnparsedPublicKey::new(&ED25519, key.0)
        .verify(message.as_bytes(), &sig)
        .map_err(|_| GrantError::BadSignature)?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| GrantError::Malformed("payload is not base64url"))?;
    let claims: Claims = serde_json::from_slice(&payload)
        .map_err(|_| GrantError::Malformed("payload is not the grant JSON"))?;
    if claims.v != 1 {
        return Err(GrantError::Malformed("unknown grant version"));
    }
    if claims.decision != "approved" {
        return Err(GrantError::NotApproved);
    }
    if claims.exp <= expect.now {
        return Err(GrantError::Expired);
    }
    if claims.action != expect.action {
        return Err(GrantError::WrongAction {
            got: claims.action,
            want: expect.action.to_owned(),
        });
    }
    if claims.payload_hash != expect.payload_hash {
        return Err(GrantError::WrongPayload);
    }
    if let Some(id) = expect.id {
        if claims.id != id {
            return Err(GrantError::WrongId);
        }
    }
    if expect.require_biometric && !claims.biometric {
        return Err(GrantError::NotBiometric);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_json_raw_and_spki() {
        let raw = [7u8; 32];
        let std = STANDARD.encode(raw);
        let k = PubKey::parse(&std).unwrap();
        assert_eq!(k.as_bytes(), &raw);
        let json = format!(r#"{{"alg":"ed25519","publicKey":"{std}","keyId":"01234567"}}"#);
        assert_eq!(PubKey::parse(&json).unwrap(), k);
        assert_eq!(PubKey::parse(&URL_SAFE_NO_PAD.encode(raw)).unwrap(), k);
        let mut spki = SPKI_PREFIX.to_vec();
        spki.extend_from_slice(&raw);
        assert_eq!(PubKey::parse(&STANDARD.encode(spki)).unwrap(), k);
        assert!(PubKey::parse("not base64 !!").is_err());
        assert!(PubKey::parse(&STANDARD.encode([1u8; 31])).is_err());
        assert!(PubKey::parse(r#"{"alg":"rsa","publicKey":"AAAA"}"#).is_err());
    }

    #[test]
    fn malformed_grants_are_rejected_before_anything_else() {
        let k = PubKey::from_bytes([1; 32]);
        let e = Expect {
            action: "root.exec",
            payload_hash: "sha256:x",
            id: None,
            require_biometric: true,
            now: 0,
        };
        assert!(matches!(
            verify(&k, "nodot", &e),
            Err(GrantError::Malformed(_))
        ));
        assert!(matches!(
            verify(&k, "a.b.c", &e),
            Err(GrantError::Malformed(_))
        ));
        assert_eq!(
            verify(&k, "e30.AAAA", &e).unwrap_err(),
            GrantError::BadSignature
        );
    }
}
