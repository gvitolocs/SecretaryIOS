//! Encrypted files in `filesDir/lucina/`.
//!
//! Each file is `LUC1 ‖ nonce (12) ‖ ciphertext ‖ tag (16)`: AES-256-GCM with
//! the 32-byte data key from the Android Keystore wrap, a fresh random
//! 96-bit nonce per write, and the file name as associated data (so
//! `queue.bin` cannot be swapped in for `state.bin`). Writes go to a 0600
//! temp file, are fsynced and renamed over the old file; the directory is
//! 0700.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use graviola::aead::AesGcm;
use graviola::hashing::hmac::Hmac;
use graviola::hashing::Sha256;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

/// Length of the data key.
pub const KEY_BYTES: usize = 32;
const MAGIC: &[u8; 4] = b"LUC1";
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const HEADER_BYTES: usize = MAGIC.len() + NONCE_BYTES;
/// State and queue are a few hundred bytes; anything far larger is not ours.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Label for the dedup fingerprint key derived from the data key.
const FINGERPRINT_LABEL: &[u8] = b"lucina-otp dedup v1";

/// Why an encrypted file could not be read.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// Wrong key, wrong file, truncated or modified.
    Tampered,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Tampered => f.write_str("tampered or wrong key"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A string wiped from memory when dropped (a token or a code).
///
/// Serializes as a plain JSON string, which only ever goes into an
/// encrypted file or the request body; `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: String) -> Self {
        Self(Zeroizing::new(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Self::new)
    }
}

/// Fills `out` from the system CSPRNG.
pub fn random_bytes(out: &mut [u8]) -> io::Result<()> {
    graviola::random::fill(out).map_err(|_| io::Error::other("rng"))
}

/// A random UUID v4, the relay's event id.
pub fn event_id() -> io::Result<String> {
    use fmt::Write as _;
    let mut b = [0u8; 16];
    random_bytes(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, x) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{x:02x}");
    }
    Ok(s)
}

/// The encrypted-file directory and its keys.
///
/// The raw data key is only used to build the AES key schedule and the
/// fingerprint key; both wipe themselves on drop.
pub struct Vault {
    dir: PathBuf,
    aead: AesGcm,
    fingerprint_key: Zeroizing<[u8; 32]>,
}

impl Vault {
    /// Opens or creates `dir` (0700). `key` must be [`KEY_BYTES`] long.
    pub fn open(dir: &Path, key: &[u8]) -> io::Result<Self> {
        if key.len() != KEY_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "data key"));
        }
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let mut mac = Hmac::<Sha256>::new(key);
        mac.update(FINGERPRINT_LABEL);
        let out = mac.finish();
        let mut fingerprint_key = Zeroizing::new([0u8; 32]);
        fingerprint_key.copy_from_slice(out.as_ref());
        Ok(Self {
            dir: dir.to_owned(),
            aead: AesGcm::new(key),
            fingerprint_key,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A keyed 64-bit fingerprint of `code`, so dedup never keeps the code.
    pub fn fingerprint(&self, code: &str) -> u64 {
        let mut mac = Hmac::<Sha256>::new(&self.fingerprint_key[..]);
        mac.update(code.as_bytes());
        let out = mac.finish();
        let mut b = [0u8; 8];
        b.copy_from_slice(&out.as_ref()[..8]);
        u64::from_be_bytes(b)
    }

    /// `plain` encrypted for the file `name`.
    pub fn seal(&self, name: &str, plain: &[u8]) -> io::Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_BYTES];
        random_bytes(&mut nonce)?;
        let mut out = Vec::with_capacity(HEADER_BYTES + plain.len() + TAG_BYTES);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(plain);
        let mut tag = [0u8; TAG_BYTES];
        self.aead
            .encrypt(&nonce, name.as_bytes(), &mut out[HEADER_BYTES..], &mut tag);
        out.extend_from_slice(&tag);
        Ok(out)
    }

    /// The plaintext of `sealed`, written for the file `name`.
    pub fn unseal(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        if sealed.len() < HEADER_BYTES + TAG_BYTES || &sealed[..MAGIC.len()] != MAGIC {
            return Err(StoreError::Tampered);
        }
        let mut nonce = [0u8; NONCE_BYTES];
        nonce.copy_from_slice(&sealed[MAGIC.len()..HEADER_BYTES]);
        let (body, tag) = sealed[HEADER_BYTES..].split_at(sealed.len() - HEADER_BYTES - TAG_BYTES);
        let mut plain = Zeroizing::new(body.to_vec());
        self.aead
            .decrypt(&nonce, name.as_bytes(), &mut plain, tag)
            .map_err(|_| StoreError::Tampered)?;
        Ok(plain)
    }

    /// Reads and decrypts `name`; `None` when it does not exist.
    pub fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        let file = match File::open(self.dir.join(name)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut sealed = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut sealed)?;
        if sealed.len() as u64 > MAX_FILE_BYTES {
            return Err(StoreError::Tampered);
        }
        self.unseal(name, &sealed).map(Some)
    }

    /// Encrypts `plain` and atomically replaces `name` (0600).
    pub fn write(&self, name: &str, plain: &[u8]) -> io::Result<()> {
        let sealed = self.seal(name, plain)?;
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.tmp"));
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&sealed)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        // Make the rename durable; a failure here leaves a valid file.
        if let Ok(d) = File::open(&self.dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// Removes `name` if it exists.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.dir.join(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}
