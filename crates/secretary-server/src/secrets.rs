//! Encryption at rest for connection secrets (Google refresh tokens, ICS
//! subscription URLs, remote MCP URLs).
//!
//! - The key is 32 random bytes in `$SECRETARY_DATA_DIR/links.key` (mode
//!   0600, created on first use with `O_EXCL`, never logged or returned).
//! - Values are sealed with ChaCha20-Poly1305 (ring) and a fresh random
//!   96-bit nonce: `enc1:` + base64url(nonce ‖ ciphertext ‖ tag).
//! - The associated data names the purpose (`google-refresh`, `ics-url:<id>`
//!   …), so a ciphertext cannot be moved to another field.
//!
//! Losing the key file loses the secrets (the owner reconnects); the rest of
//! the data stays readable.

use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};

pub const KEY_FILE: &str = "links.key";
const PREFIX: &str = "enc1:";

pub struct Sealer {
    path: PathBuf,
    key: OnceLock<LessSafeKey>,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer").field("path", &self.path).finish()
    }
}

fn load_or_create(path: &Path) -> io::Result<[u8; 32]> {
    if let Some(dir) = path.parent() {
        crate::devices::ensure_dir(dir)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut f) => {
            let mut key = [0u8; 32];
            getrandom::fill(&mut key).map_err(|e| io::Error::other(e.to_string()))?;
            f.write_all(&key)?;
            f.sync_all()?;
            Ok(key)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let meta = std::fs::metadata(path)?;
            if meta.permissions().mode() & 0o077 != 0 {
                tracing::warn!(path = %path.display(), "links.key is readable by group/others; chmod 600 it");
            }
            let mut buf = Vec::new();
            std::fs::File::open(path)?.read_to_end(&mut buf)?;
            buf.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "links.key must be exactly 32 bytes",
                )
            })
        }
        Err(e) => Err(e),
    }
}

impl Sealer {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(KEY_FILE),
            key: OnceLock::new(),
        }
    }

    fn key(&self) -> io::Result<&LessSafeKey> {
        if let Some(k) = self.key.get() {
            return Ok(k);
        }
        let raw = load_or_create(&self.path)?;
        let unbound =
            UnboundKey::new(&CHACHA20_POLY1305, &raw).map_err(|_| io::Error::other("bad key"))?;
        let _ = self.key.set(LessSafeKey::new(unbound));
        Ok(self.key.get().expect("key set"))
    }

    /// Encrypt `plain` for `purpose`.
    pub fn seal(&self, purpose: &str, plain: &str) -> io::Result<String> {
        let key = self.key()?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
        let mut buf = plain.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(purpose.as_bytes()),
            &mut buf,
        )
        .map_err(|_| io::Error::other("seal failed"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buf);
        Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(out)))
    }

    /// Decrypt a value sealed for `purpose`.
    pub fn open(&self, purpose: &str, sealed: &str) -> io::Result<String> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "sealed value does not open");
        let raw = sealed
            .strip_prefix(PREFIX)
            .and_then(|b| URL_SAFE_NO_PAD.decode(b).ok())
            .filter(|b| b.len() > NONCE_LEN)
            .ok_or_else(bad)?;
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| bad())?;
        let mut buf = ct.to_vec();
        let plain = self
            .key()?
            .open_in_place(nonce, Aad::from(purpose.as_bytes()), &mut buf)
            .map_err(|_| bad())?;
        String::from_utf8(plain.to_vec()).map_err(|_| bad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_purpose_binding_and_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sealer::new(dir.path());
        let sealed = s.seal("google-refresh", "1//secret-token").unwrap();
        assert!(sealed.starts_with("enc1:") && !sealed.contains("secret"));
        assert_ne!(sealed, s.seal("google-refresh", "1//secret-token").unwrap());
        assert_eq!(
            s.open("google-refresh", &sealed).unwrap(),
            "1//secret-token"
        );
        assert!(s.open("ics-url:x", &sealed).is_err());
        let mode = std::fs::metadata(dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // a second instance (restart) reads the same key
        assert_eq!(
            Sealer::new(dir.path())
                .open("google-refresh", &sealed)
                .unwrap(),
            "1//secret-token"
        );
        // tampering fails
        let mut t = sealed.clone();
        t.pop();
        t.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert!(s.open("google-refresh", &t).is_err());
        assert!(s.open("google-refresh", "plain").is_err());
    }
}
