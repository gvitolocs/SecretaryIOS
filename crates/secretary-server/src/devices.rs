//! Device tokens.
//!
//! A token is `sec_` + base64url(32 random bytes). Only its SHA-256 (hex) is
//! stored, in `$SECRETARY_DATA_DIR/devices.json`:
//! `[{id, name, hash, createdAt, lastSeenAt, revoked}]`.
//!
//! Every mutation (CLI issue/revoke, the server's `lastSeenAt` touch) is a
//! read-modify-write of the file under an exclusive lock on `devices.lock`,
//! written to a temp file (0600), fsynced and renamed over the old one. The
//! server re-reads the file whenever its mtime/size changes, so a CLI revoke
//! takes effect on the next request without a restart.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const TOKEN_PREFIX: &str = "sec_";
const DEVICES_FILE: &str = "devices.json";
const LOCK_FILE: &str = "devices.lock";
/// Persist `lastSeenAt` at most this often per device.
const TOUCH_EVERY: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Hex SHA-256 of the full token string.
    pub hash: String,
    pub created_at: String,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub revoked: bool,
    /// The device that browser sessions from an email + password login
    /// hang off (named "web"; its token is never shown to anyone).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web_login: bool,
    /// A test client (an agent's probe, a test rig): its turns go to Lucina's self-test
    /// conversation, never the owner's chat, and never notify (docs/notifications.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(buf)
}

/// A fresh device token: `sec_` + base64url(32 random bytes).
pub fn generate_token() -> io::Result<String> {
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(random_bytes::<32>()?)
    ))
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn validate_name(name: &str) -> io::Result<()> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device name must be 1..=64 printable characters",
        ));
    }
    Ok(())
}

// -- shared file helpers (also used by `sessions`) ---------------------------

pub(crate) fn ensure_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    Ok(())
}

/// Exclusive lock on `dir/name`, held for the lifetime of the returned file.
pub(crate) fn lock_file(dir: &Path, name: &str) -> io::Result<File> {
    ensure_dir(dir)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(name))?;
    f.lock()?;
    Ok(f)
}

/// A JSON array file; missing means empty.
pub(crate) fn load_json_vec<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Write `value` to `dir/name` atomically: temp file 0600, fsync, rename,
/// fsync the directory.
pub(crate) fn save_json_atomic<T: Serialize + ?Sized>(
    dir: &Path,
    name: &str,
    value: &T,
) -> io::Result<()> {
    ensure_dir(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    let mut json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    json.push(b'\n');
    f.write_all(&json)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(name))?;
    File::open(dir)?.sync_all()
}

/// mtime + size of a file, `None` when missing: the reload trigger.
pub(crate) fn file_stamp(path: &Path) -> io::Result<Option<(SystemTime, u64)>> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some((m.modified()?, m.len()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub(crate) fn random_b64<const N: usize>() -> io::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<N>()?))
}

/// The on-disk registry.
#[derive(Debug, Clone)]
pub struct DeviceStore {
    dir: PathBuf,
}

impl DeviceStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(DEVICES_FILE)
    }

    fn lock(&self) -> io::Result<File> {
        lock_file(&self.dir, LOCK_FILE)
    }

    /// All devices; a missing file is an empty registry.
    pub fn load(&self) -> io::Result<Vec<Device>> {
        load_json_vec(&self.path())
    }

    fn save(&self, devices: &[Device]) -> io::Result<()> {
        save_json_atomic(&self.dir, DEVICES_FILE, devices)
    }

    /// Register a device; returns it and the plaintext token (shown once).
    pub fn issue(&self, name: &str) -> io::Result<(Device, String)> {
        validate_name(name)?;
        let _lock = self.lock()?;
        let mut devices = self.load()?;
        let token = generate_token()?;
        let device = Device {
            id: format!("dev_{}", URL_SAFE_NO_PAD.encode(random_bytes::<8>()?)),
            name: name.trim().to_owned(),
            hash: hash_token(&token),
            created_at: now_rfc3339(),
            last_seen_at: None,
            revoked: false,
            web_login: false,
            test: false,
        };
        devices.push(device.clone());
        self.save(&devices)?;
        Ok((device, token))
    }

    /// The active `web_login` device, created on first use.
    pub fn ensure_web_login(&self) -> io::Result<Device> {
        let _lock = self.lock()?;
        let mut devices = self.load()?;
        if let Some(d) = devices.iter().find(|d| d.web_login && !d.revoked) {
            return Ok(d.clone());
        }
        let device = Device {
            id: format!("dev_{}", URL_SAFE_NO_PAD.encode(random_bytes::<8>()?)),
            name: "web".to_owned(),
            // hash of a token nobody ever sees: Bearer use is impossible
            hash: hash_token(&generate_token()?),
            created_at: now_rfc3339(),
            last_seen_at: None,
            revoked: false,
            web_login: true,
            test: false,
        };
        devices.push(device.clone());
        self.save(&devices)?;
        Ok(device)
    }

    pub fn list(&self) -> io::Result<Vec<Device>> {
        self.load()
    }

    /// Mark `id` revoked. `Ok(None)` if there is no such device.
    pub fn revoke(&self, id: &str) -> io::Result<Option<Device>> {
        let _lock = self.lock()?;
        let mut devices = self.load()?;
        let Some(d) = devices.iter_mut().find(|d| d.id == id) else {
            return Ok(None);
        };
        d.revoked = true;
        let out = d.clone();
        self.save(&devices)?;
        Ok(Some(out))
    }

    /// Mark `id` a test device (or not). `Ok(None)` if there is no such device.
    pub fn set_test(&self, id: &str, test: bool) -> io::Result<Option<Device>> {
        let _lock = self.lock()?;
        let mut devices = self.load()?;
        let Some(d) = devices.iter_mut().find(|d| d.id == id) else {
            return Ok(None);
        };
        d.test = test;
        let out = d.clone();
        self.save(&devices)?;
        Ok(Some(out))
    }

    /// Record `lastSeenAt` for `id`, re-reading the file under the lock so a
    /// concurrent revoke is never overwritten.
    pub fn touch(&self, id: &str, at: &str) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut devices = self.load()?;
        if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
            d.last_seen_at = Some(at.to_owned());
            self.save(&devices)?;
        }
        Ok(())
    }
}

/// Result of checking a presented token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthResult {
    Ok { device_id: String, name: String },
    Unknown,
    Revoked,
}

#[derive(Default)]
struct Cache {
    stamp: Option<(SystemTime, u64)>,
    loaded: bool,
    devices: Vec<Device>,
    touched: HashMap<String, Instant>,
}

/// Server-side view of the registry, reloaded when the file changes.
pub struct DeviceAuth {
    store: DeviceStore,
    cache: Mutex<Cache>,
}

impl DeviceAuth {
    pub fn new(store: DeviceStore) -> Self {
        Self {
            store,
            cache: Mutex::new(Cache::default()),
        }
    }

    fn stamp(&self) -> io::Result<Option<(SystemTime, u64)>> {
        file_stamp(&self.store.path())
    }

    pub fn store(&self) -> &DeviceStore {
        &self.store
    }

    fn refreshed(&self) -> io::Result<std::sync::MutexGuard<'_, Cache>> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let stamp = self.stamp()?;
        if !cache.loaded || cache.stamp != stamp {
            cache.devices = self.store.load()?;
            cache.stamp = stamp;
            cache.loaded = true;
        }
        Ok(cache)
    }

    /// Current state of device `id` (used by cookie sessions, so a revoke
    /// takes effect on sessions too). Records `lastSeenAt` when active.
    pub fn active_device(&self, id: &str) -> io::Result<Option<Device>> {
        let mut cache = self.refreshed()?;
        let found = cache
            .devices
            .iter()
            .find(|d| d.id == id && !d.revoked)
            .cloned();
        if found.is_some() {
            self.mark_seen(&mut cache, id);
        }
        Ok(found)
    }

    /// Whether `id` is flagged `test` (an unreadable registry counts as not).
    pub fn is_test(&self, id: &str) -> bool {
        self.refreshed()
            .map(|c| c.devices.iter().any(|d| d.id == id && d.test))
            .unwrap_or(false)
    }

    fn mark_seen(&self, cache: &mut Cache, device_id: &str) {
        let due = cache
            .touched
            .get(device_id)
            .is_none_or(|t| t.elapsed() >= TOUCH_EVERY);
        if due {
            cache.touched.insert(device_id.to_owned(), Instant::now());
            let store = self.store.clone();
            let id = device_id.to_owned();
            // Best effort, off the request path.
            tokio::task::spawn_blocking(move || {
                if let Err(e) = store.touch(&id, &now_rfc3339()) {
                    tracing::warn!(error = %e, "could not persist lastSeenAt");
                }
            });
        }
    }

    /// Check `token` against every stored hash in constant time per entry
    /// (no early exit), after reloading the registry if it changed on disk.
    /// Errors (unreadable/corrupt registry) must be treated as a denial.
    pub fn authenticate(&self, token: &str) -> io::Result<AuthResult> {
        let presented = hash_token(token);
        let mut cache = self.refreshed()?;
        let mut found: Option<&Device> = None;
        for d in &cache.devices {
            let eq: bool = d.hash.as_bytes().ct_eq(presented.as_bytes()).into();
            if eq {
                found = Some(d);
            }
        }
        let result = match found {
            None => AuthResult::Unknown,
            Some(d) if d.revoked => AuthResult::Revoked,
            Some(d) => AuthResult::Ok {
                device_id: d.id.clone(),
                name: d.name.clone(),
            },
        };
        if let AuthResult::Ok { device_id, .. } = &result {
            let id = device_id.clone();
            self.mark_seen(&mut cache, &id);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn token_shape() {
        let t = generate_token().unwrap();
        assert!(t.starts_with(TOKEN_PREFIX));
        // 32 bytes base64url without padding = 43 chars
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 43);
        assert_ne!(t, generate_token().unwrap());
        assert_eq!(hash_token("x").len(), 64);
    }

    #[test]
    fn issue_revoke_roundtrip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::new(dir.path().join("data"));
        let (dev, token) = store.issue("iPhone").unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let raw = fs::read_to_string(store.path()).unwrap();
        assert!(
            !raw.contains(&token),
            "plaintext token must never be stored"
        );
        assert!(raw.contains(&hash_token(&token)));

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, dev.id);
        assert!(store.issue("  ").is_err());

        assert!(store.revoke("dev_missing").unwrap().is_none());
        assert!(store.revoke(&dev.id).unwrap().unwrap().revoked);
        assert!(store.list().unwrap()[0].revoked);
    }
}
