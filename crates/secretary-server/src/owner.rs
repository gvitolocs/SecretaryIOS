//! The single owner account: email + password login.
//!
//! Stored in `$SECRETARY_DATA_DIR/owner.json` (0600, atomic writes, locked
//! read-modify-write like `devices.json`):
//! `{email, password_hash, created_at, updated_at, failed_count, locked_until}`.
//!
//! - `password_hash` is an Argon2id PHC string (crate defaults: m = 19 MiB,
//!   t = 2, p = 1). Verification always runs one Argon2 verify, against a
//!   dummy hash when the email does not match or no owner exists, so the
//!   response time does not reveal whether an email is the owner's.
//! - Lockout counts every failed password attempt (whatever email was
//!   typed, so a lock does not reveal the email either): the 10th
//!   consecutive failure locks password logins for 15 minutes, and every
//!   further failure after a lock doubles it (30 min, 1 h, … up to 24 h).
//!   A success resets the count. Device-token login is never locked.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::devices::{lock_file, now_rfc3339, save_json_atomic};

const OWNER_FILE: &str = "owner.json";
const LOCK_FILE: &str = "owner.lock";
pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_BYTES: usize = 1024;
pub const LOCK_AFTER: u32 = 10;
pub const FIRST_LOCK: Duration = Duration::from_secs(15 * 60);
pub const MAX_LOCK: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    /// Lowercased.
    pub email: String,
    /// Argon2id PHC string.
    pub password_hash: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub failed_count: u32,
    /// Unix seconds; password logins are refused until then.
    #[serde(default)]
    pub locked_until: Option<u64>,
}

/// What a password check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Invalid,
    /// Locked; retry after this long.
    Locked(Duration),
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn normalize_email(email: &str) -> io::Result<String> {
    let e = email.trim().to_lowercase();
    let valid = (3..=254).contains(&e.len())
        && e.split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        && !e.chars().any(|c| c.is_whitespace() || c.is_control());
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "email must look like name@example.com",
        ));
    }
    Ok(e)
}

pub fn check_new_password(password: &str) -> io::Result<()> {
    if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "password must be at least {MIN_PASSWORD_CHARS} characters (at most {MAX_PASSWORD_BYTES} bytes)"
            ),
        ));
    }
    Ok(())
}

pub fn hash_password(password: &str) -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    let salt = SaltString::encode_b64(&bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| io::Error::other(e.to_string()))
}

fn verify_hash(password: &str, phc: &str) -> bool {
    PasswordHash::new(phc).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// A hash of a random password with the default parameters, so a dummy
/// verify costs the same as a real one.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let mut random = [0u8; 24];
        let _ = getrandom::fill(&mut random);
        hash_password(&format!("{random:?}")).unwrap_or_default()
    })
}

/// Compute the dummy hash now (call once at startup, off the request path).
pub fn warm_up() {
    let _ = dummy_hash();
}

fn same_email(a: &str, b: &str) -> bool {
    let (a, b) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    a.ct_eq(&b).into()
}

fn lock_for(failed_count: u32) -> Duration {
    let doublings = failed_count.saturating_sub(LOCK_AFTER).min(16);
    (FIRST_LOCK * 2u32.pow(doublings)).min(MAX_LOCK)
}

#[derive(Debug, Clone)]
pub struct OwnerStore {
    dir: PathBuf,
}

impl OwnerStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(OWNER_FILE)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn load(&self) -> io::Result<Option<Owner>> {
        match std::fs::read(self.path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn save(&self, owner: &Owner) -> io::Result<()> {
        save_json_atomic(&self.dir, OWNER_FILE, owner)
    }

    /// Create or update the one owner (`user set`). A different email than
    /// the stored one is refused unless `replace`.
    pub fn set(&self, email: &str, password: &str, replace: bool) -> io::Result<Owner> {
        let email = normalize_email(email)?;
        check_new_password(password)?;
        let _lock = lock_file(&self.dir, LOCK_FILE)?;
        let existing = self.load()?;
        if let Some(o) = &existing {
            if o.email != email && !replace {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "an owner account already exists ({}); there is exactly one user. Pass --replace to replace it",
                        o.email
                    ),
                ));
            }
        }
        let now = now_rfc3339();
        let owner = Owner {
            created_at: existing
                .filter(|o| o.email == email)
                .map_or_else(|| now.clone(), |o| o.created_at),
            email,
            password_hash: hash_password(password)?,
            updated_at: now,
            failed_count: 0,
            locked_until: None,
        };
        self.save(&owner)?;
        Ok(owner)
    }

    /// Check `email` + `password` and record the outcome (failure count,
    /// lockout). Always costs one Argon2 verify unless locked.
    pub fn verify(&self, email: &str, password: &str) -> io::Result<Verdict> {
        self.verify_at(email, password, unix_now())
    }

    pub fn verify_at(&self, email: &str, password: &str, now: u64) -> io::Result<Verdict> {
        let _lock = lock_file(&self.dir, LOCK_FILE)?;
        let Some(mut owner) = self.load()? else {
            verify_hash(password, dummy_hash());
            return Ok(Verdict::Invalid);
        };
        if let Some(until) = owner.locked_until.filter(|u| *u > now) {
            return Ok(Verdict::Locked(Duration::from_secs(until - now)));
        }
        let typed = email.trim().to_lowercase();
        let email_ok = same_email(&typed, &owner.email);
        let target = if email_ok {
            owner.password_hash.as_str()
        } else {
            dummy_hash()
        };
        let password_ok = password.len() <= MAX_PASSWORD_BYTES && verify_hash(password, target);
        if email_ok && password_ok {
            if owner.failed_count != 0 || owner.locked_until.is_some() {
                owner.failed_count = 0;
                owner.locked_until = None;
                self.save(&owner)?;
            }
            return Ok(Verdict::Ok);
        }
        owner.failed_count = owner.failed_count.saturating_add(1);
        if owner.failed_count >= LOCK_AFTER {
            owner.locked_until = Some(now + lock_for(owner.failed_count).as_secs());
        }
        self.save(&owner)?;
        Ok(Verdict::Invalid)
    }

    /// Replace the password of the existing owner (after the caller verified
    /// the current one).
    pub fn change_password(&self, new_password: &str) -> io::Result<()> {
        check_new_password(new_password)?;
        let _lock = lock_file(&self.dir, LOCK_FILE)?;
        let Some(mut owner) = self.load()? else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no owner account"));
        };
        owner.password_hash = hash_password(new_password)?;
        owner.updated_at = now_rfc3339();
        owner.failed_count = 0;
        owner.locked_until = None;
        self.save(&owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const PW: &str = "correct horse battery";

    #[test]
    fn set_verify_and_single_user_rule() {
        let dir = tempfile::tempdir().unwrap();
        let store = OwnerStore::new(dir.path());
        assert_eq!(store.verify("a@b.c", PW).unwrap(), Verdict::Invalid);
        assert!(store.set("Owner@Example.com", "short", false).is_err());
        assert!(store.set("not-an-email", PW, false).is_err());
        let owner = store.set(" Owner@Example.com ", PW, false).unwrap();
        assert_eq!(owner.email, "owner@example.com");
        assert!(owner
            .password_hash
            .starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(!raw.contains(PW));
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        assert_eq!(store.verify("OWNER@example.com", PW).unwrap(), Verdict::Ok);
        assert_eq!(
            store
                .verify("owner@example.com", "wrong password!")
                .unwrap(),
            Verdict::Invalid
        );
        assert_eq!(
            store.verify("other@example.com", PW).unwrap(),
            Verdict::Invalid
        );
        assert_eq!(store.load().unwrap().unwrap().failed_count, 2);
        assert_eq!(store.verify("owner@example.com", PW).unwrap(), Verdict::Ok);
        assert_eq!(store.load().unwrap().unwrap().failed_count, 0);

        // exactly one user
        let err = store.set("second@example.com", PW, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        // same email: password update keeps created_at
        let again = store
            .set("owner@example.com", "another long password", false)
            .unwrap();
        assert_eq!(again.created_at, owner.created_at);
        assert_eq!(
            store.verify("owner@example.com", PW).unwrap(),
            Verdict::Invalid
        );
        let replaced = store.set("second@example.com", PW, true).unwrap();
        assert_eq!(replaced.email, "second@example.com");
        assert_eq!(
            store.verify("owner@example.com", PW).unwrap(),
            Verdict::Invalid
        );
        assert_eq!(store.verify("second@example.com", PW).unwrap(), Verdict::Ok);
    }

    #[test]
    fn lockout_after_ten_failures_doubles_and_resets() {
        assert_eq!(lock_for(10), FIRST_LOCK);
        assert_eq!(lock_for(11), FIRST_LOCK * 2);
        assert_eq!(lock_for(12), FIRST_LOCK * 4);
        assert_eq!(lock_for(40), MAX_LOCK);
        let dir = tempfile::tempdir().unwrap();
        let store = OwnerStore::new(dir.path());
        store.set("owner@example.com", PW, false).unwrap();
        let t0 = 1_800_000_000;
        for i in 0..9 {
            assert_eq!(
                store
                    .verify_at("owner@example.com", "nope nope nope", t0 + i)
                    .unwrap(),
                Verdict::Invalid
            );
        }
        // the 10th failure locks (wrong emails count too)
        assert_eq!(
            store.verify_at("x@y.z", "nope", t0 + 9).unwrap(),
            Verdict::Invalid
        );
        let locked = store.verify_at("owner@example.com", PW, t0 + 10).unwrap();
        assert_eq!(locked, Verdict::Locked(Duration::from_secs(15 * 60 - 1)));
        // after the lock, one more failure locks for 30 min
        let t1 = t0 + 9 + 15 * 60;
        assert_eq!(
            store.verify_at("owner@example.com", "nope", t1).unwrap(),
            Verdict::Invalid
        );
        assert_eq!(
            store.verify_at("owner@example.com", PW, t1).unwrap(),
            Verdict::Locked(Duration::from_secs(30 * 60))
        );
        // success after expiry resets everything
        let t2 = t1 + 30 * 60;
        assert_eq!(
            store.verify_at("owner@example.com", PW, t2).unwrap(),
            Verdict::Ok
        );
        let o = store.load().unwrap().unwrap();
        assert_eq!((o.failed_count, o.locked_until), (0, None));
    }

    #[test]
    fn change_password_resets_and_requires_length() {
        let dir = tempfile::tempdir().unwrap();
        let store = OwnerStore::new(dir.path());
        assert!(store.change_password("a brand new password").is_err());
        store.set("owner@example.com", PW, false).unwrap();
        assert!(store.change_password("short").is_err());
        store.change_password("a brand new password").unwrap();
        assert_eq!(
            store.verify("owner@example.com", PW).unwrap(),
            Verdict::Invalid
        );
        assert_eq!(
            store
                .verify("owner@example.com", "a brand new password")
                .unwrap(),
            Verdict::Ok
        );
    }
}
