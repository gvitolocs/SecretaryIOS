//! Browser cookie sessions.
//!
//! `POST /v1/session {token}` exchanges a valid device token for a session
//! id (32 random bytes, base64url) set as the `sec_session` cookie. Only the
//! SHA-256 of the id is stored, in `$SECRETARY_DATA_DIR/sessions.json`
//! (`[{hash, deviceId, createdAt, expiresAt}]`, 0600, atomic, locked like
//! `devices.json`). A session is valid while it is unexpired **and** its
//! device is active, so revoking the device ends its sessions immediately;
//! `token revoke` also deletes them from the file.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::devices::{
    file_stamp, hash_token, load_json_vec, lock_file, now_rfc3339, random_b64, save_json_atomic,
};

pub const COOKIE_NAME: &str = "sec_session";
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
const SESSIONS_FILE: &str = "sessions.json";
const LOCK_FILE: &str = "sessions.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Hex SHA-256 of the session id.
    pub hash: String,
    pub device_id: String,
    pub created_at: String,
    /// Unix seconds.
    pub expires_at: u64,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(SESSIONS_FILE)
    }

    pub fn load(&self) -> io::Result<Vec<Session>> {
        load_json_vec(&self.path())
    }

    fn modify<R>(&self, f: impl FnOnce(&mut Vec<Session>) -> R) -> io::Result<R> {
        let _lock = lock_file(&self.dir, LOCK_FILE)?;
        let mut sessions = self.load()?;
        let before = sessions.clone();
        let out = f(&mut sessions);
        if sessions != before {
            save_json_atomic(&self.dir, SESSIONS_FILE, &sessions)?;
        }
        Ok(out)
    }

    /// New session for `device_id`; returns the plaintext id (cookie value).
    /// Expired sessions are pruned on the way.
    pub fn create(&self, device_id: &str) -> io::Result<String> {
        let id = random_b64::<32>()?;
        let now = unix_now();
        let session = Session {
            hash: hash_token(&id),
            device_id: device_id.to_owned(),
            created_at: now_rfc3339(),
            expires_at: now + SESSION_TTL.as_secs(),
        };
        self.modify(|s| {
            s.retain(|x| x.expires_at > now);
            s.push(session);
        })?;
        Ok(id)
    }

    /// Delete the session with this plaintext id (logout).
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let hash = hash_token(id);
        self.modify(|s| {
            let n = s.len();
            s.retain(|x| x.hash != hash);
            s.len() != n
        })
    }

    /// Delete every session of a device (on revoke). Returns how many.
    pub fn delete_for_device(&self, device_id: &str) -> io::Result<usize> {
        self.modify(|s| {
            let n = s.len();
            s.retain(|x| x.device_id != device_id);
            n - s.len()
        })
    }
}

#[derive(Default)]
struct Cache {
    stamp: Option<(SystemTime, u64)>,
    loaded: bool,
    sessions: Vec<Session>,
}

/// Server-side view of `sessions.json`, reloaded when the file changes.
pub struct SessionAuth {
    store: SessionStore,
    cache: Mutex<Cache>,
}

impl SessionAuth {
    pub fn new(store: SessionStore) -> Self {
        Self {
            store,
            cache: Mutex::new(Cache::default()),
        }
    }

    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// Device id of the unexpired session with plaintext id `id`; every
    /// stored hash is compared in constant time (no early exit).
    pub fn device_for(&self, id: &str) -> io::Result<Option<String>> {
        let presented = hash_token(id);
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let stamp = file_stamp(&self.store.path())?;
        if !cache.loaded || cache.stamp != stamp {
            cache.sessions = self.store.load()?;
            cache.stamp = stamp;
            cache.loaded = true;
        }
        let now = unix_now();
        let mut found = None;
        for s in &cache.sessions {
            let eq: bool = s.hash.as_bytes().ct_eq(presented.as_bytes()).into();
            if eq && s.expires_at > now {
                found = Some(s.device_id.clone());
            }
        }
        Ok(found)
    }
}

/// Revoke a device and delete its sessions (what `token revoke` runs).
pub fn revoke_device(
    dir: &Path,
    device_id: &str,
) -> io::Result<Option<(crate::devices::Device, usize)>> {
    let Some(dev) = crate::devices::DeviceStore::new(dir).revoke(device_id)? else {
        return Ok(None);
    };
    let n = SessionStore::new(dir).delete_for_device(device_id)?;
    Ok(Some((dev, n)))
}

/// `Set-Cookie` value for a fresh session.
pub fn set_cookie(id: &str) -> String {
    format!(
        "{COOKIE_NAME}={id}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL.as_secs()
    )
}

/// `Set-Cookie` value that clears the session cookie.
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0")
}

/// The `sec_session` value from the request's `Cookie` headers, if any.
pub fn cookie_value(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE_NAME)
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn create_lookup_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let auth = SessionAuth::new(store.clone());
        let id = store.create("dev_a").unwrap();
        assert_eq!(id.len(), 43);
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(
            !raw.contains(&id),
            "plaintext session id must not be stored"
        );
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        assert_eq!(auth.device_for(&id).unwrap().as_deref(), Some("dev_a"));
        assert_eq!(auth.device_for("nope").unwrap(), None);
        let other = store.create("dev_b").unwrap();
        assert_eq!(store.delete_for_device("dev_a").unwrap(), 1);
        assert_eq!(auth.device_for(&id).unwrap(), None);
        assert!(store.delete(&other).unwrap());
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn cookie_parsing() {
        let mut h = axum::http::HeaderMap::new();
        h.append("cookie", "a=1; sec_session=abc ; b=2".parse().unwrap());
        assert_eq!(cookie_value(&h).as_deref(), Some("abc"));
        let mut h = axum::http::HeaderMap::new();
        h.append("cookie", "sec_session=".parse().unwrap());
        assert_eq!(cookie_value(&h), None);
        assert!(
            set_cookie("x").contains("HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=2592000")
        );
    }
}
