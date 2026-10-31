//! Service tokens (`mcp_…`) for Claude Code, Grok and scripts.
//!
//! Stored in `$SECRETARY_MCP_DATA_DIR/tokens.json` (0600) as
//! `[{id, name, hash, scopes, created_at, last_used_at, revoked}]`; only the
//! SHA-256 of a token is kept. A token is `mcp_` + 43 base64url characters
//! (256 random bits) and is shown once, by `secretary-mcp token issue`.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::scope::{valid_caller_name, Caller, CallerKind, Scopes};
use crate::store::{random_id, random_secret, sha256_hex, unix_now, JsonFile};

pub const TOKEN_PREFIX: &str = "mcp_";
const FILE: &str = "tokens.json";
const LOCK: &str = "tokens.lock";
/// `last_used_at` is written at most this often per token.
const TOUCH_EVERY_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceToken {
    pub id: String,
    /// The caller name Lucina sees (`claude-code`, `chatgpt`, `grok`, …).
    pub name: String,
    /// SHA-256 hex of the token.
    pub hash: String,
    pub scopes: Scopes,
    pub created_at: u64,
    #[serde(default)]
    pub last_used_at: Option<u64>,
    #[serde(default)]
    pub revoked: bool,
    /// Delegation depth of whoever holds it ([`Caller::depth`]); 0 is omitted.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub depth: u8,
}

fn is_zero(d: &u8) -> bool {
    *d == 0
}

pub struct TokenStore {
    file: JsonFile<Vec<ServiceToken>>,
}

impl TokenStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            file: JsonFile::new(dir, FILE, LOCK),
        }
    }

    /// Issue a new token; returns the record and the plaintext (shown once).
    pub fn issue(&self, name: &str, scopes: Scopes) -> io::Result<(ServiceToken, String)> {
        self.issue_at(name, scopes, 0)
    }

    /// [`Self::issue`] for a holder at delegation depth `depth` (0 or 1): an
    /// agent run gets a depth-1 token, which cannot start further agents.
    pub fn issue_at(&self, name: &str, scopes: Scopes, depth: u8) -> io::Result<(ServiceToken, String)> {
        if depth > crate::agents::MAX_DEPTH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("depth must be 0-{}", crate::agents::MAX_DEPTH),
            ));
        }
        if !valid_caller_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "name must be 1-32 characters of a-z, 0-9 and '-' (e.g. claude-code, chatgpt, grok)",
            ));
        }
        if scopes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "at least one scope is needed (read, chat, write, computer, agents, notebooks)",
            ));
        }
        let secret = format!("{TOKEN_PREFIX}{}", random_secret()?);
        let rec = ServiceToken {
            id: random_id("tok_")?,
            name: name.to_owned(),
            hash: sha256_hex(&secret),
            scopes,
            created_at: unix_now(),
            last_used_at: None,
            revoked: false,
            depth,
        };
        let out = rec.clone();
        self.file.update(move |v| {
            v.push(rec);
            (true, ())
        })?;
        Ok((out, secret))
    }

    pub fn list(&self) -> io::Result<Vec<ServiceToken>> {
        self.file.read(Clone::clone)
    }

    /// Add `add` to an active token's scopes (by id, or every active token with
    /// that name). Returns the updated records.
    pub fn add_scopes(&self, id_or_name: &str, add: &Scopes) -> io::Result<Vec<ServiceToken>> {
        self.file.update(|v| {
            let mut hit = Vec::new();
            for t in v.iter_mut() {
                if !t.revoked && (t.id == id_or_name || t.name == id_or_name) {
                    t.scopes = Scopes::of(t.scopes.iter().chain(add.iter()));
                    hit.push(t.clone());
                }
            }
            (!hit.is_empty(), hit)
        })
    }

    /// Revoke by id, or by name (every active token with that name).
    /// Returns the revoked records.
    pub fn revoke(&self, id_or_name: &str) -> io::Result<Vec<ServiceToken>> {
        self.file.update(|v| {
            let mut hit = Vec::new();
            for t in v.iter_mut() {
                if !t.revoked && (t.id == id_or_name || t.name == id_or_name) {
                    t.revoked = true;
                    hit.push(t.clone());
                }
            }
            (!hit.is_empty(), hit)
        })
    }

    /// The caller for a presented `mcp_…` token, if it is known and active.
    pub fn authenticate(&self, token: &str) -> io::Result<Option<Caller>> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let hash = sha256_hex(token);
        let found = self.file.read(|v| {
            v.iter()
                .find(|t| bool::from(t.hash.as_bytes().ct_eq(hash.as_bytes())))
                .cloned()
        })?;
        let Some(t) = found.filter(|t| !t.revoked) else {
            return Ok(None);
        };
        let now = unix_now();
        if t.last_used_at
            .is_none_or(|at| now.saturating_sub(at) >= TOUCH_EVERY_SECS)
        {
            // Best effort: a failed touch must not fail the request.
            let id = t.id.clone();
            let _ = self.file.update(|v| {
                let mut changed = false;
                if let Some(rec) = v.iter_mut().find(|r| r.id == id) {
                    rec.last_used_at = Some(now);
                    changed = true;
                }
                (changed, ())
            });
        }
        Ok(Some(Caller {
            name: t.name,
            kind: CallerKind::ServiceToken,
            scopes: t.scopes,
            id: t.id,
            depth: t.depth,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_authenticate_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path());
        let (rec, secret) = store
            .issue("claude-code", Scopes::parse_list("read,chat").unwrap())
            .unwrap();
        assert!(secret.starts_with("mcp_") && secret.len() == 4 + 43);
        let c = store.authenticate(&secret).unwrap().unwrap();
        assert_eq!(c.name, "claude-code");
        assert_eq!(c.scopes.to_string(), "read,chat");
        assert!(store.authenticate("mcp_nope").unwrap().is_none());
        assert!(store.authenticate("sec_x").unwrap().is_none());
        // the file never holds the token itself
        let raw = std::fs::read_to_string(dir.path().join("tokens.json")).unwrap();
        assert!(!raw.contains(&secret[4..]));
        assert_eq!(store.revoke(&rec.id).unwrap().len(), 1);
        assert!(store.authenticate(&secret).unwrap().is_none());
        assert!(store.revoke(&rec.id).unwrap().is_empty());
        assert!(store.issue("Bad Name", Scopes::all()).is_err());
        assert!(store.issue("grok", Scopes::default()).is_err());
        let (_, s1) = store.issue_at("hermes-run", Scopes::parse_list("read,agents").unwrap(), 1).unwrap();
        assert_eq!(store.authenticate(&s1).unwrap().unwrap().depth, 1);
        assert!(store.issue_at("deep", Scopes::all(), 2).is_err());
        // depth 0 is not written, so older readers see the same records
        let raw = std::fs::read_to_string(dir.path().join("tokens.json")).unwrap();
        assert_eq!(raw.matches("\"depth\"").count(), 1);
    }
}
