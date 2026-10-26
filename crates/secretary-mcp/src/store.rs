//! Small persistence helpers shared by the token and OAuth stores.
//!
//! Every store is one JSON file in `SECRETARY_MCP_DATA_DIR` (directory 0700,
//! files 0600), written atomically (temp file, fsync, rename, fsync dir) under
//! an exclusive `flock`-style lock, so the CLI (`token issue` / `revoke`) and
//! the running server can change the same file safely. Readers reload when
//! the file's mtime or size changes, so a revoke from the CLI takes effect on
//! the next request without a restart.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// RFC 3339 UTC timestamp (for humans in `token list`).
pub fn rfc3339(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// `N` random bytes from the OS.
pub fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(buf)
}

/// base64url (no padding) of 32 random bytes: 43 characters, 256 bits.
pub fn random_secret() -> io::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<32>()?))
}

/// A short random id (`<prefix>` + 16 base64url chars, 96 bits).
pub fn random_id(prefix: &str) -> io::Result<String> {
    Ok(format!(
        "{prefix}{}",
        URL_SAFE_NO_PAD.encode(random_bytes::<12>()?)
    ))
}

/// Lowercase hex SHA-256: how every secret is stored.
pub fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// base64url(SHA-256(s)): the PKCE S256 transform.
pub fn sha256_b64url(s: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(s.as_bytes()))
}

pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    Ok(())
}

/// Exclusive lock on `dir/name`, held while the returned file lives.
fn lock_file(dir: &Path, name: &str) -> io::Result<File> {
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

fn save_atomic<T: Serialize>(dir: &Path, name: &str, value: &T) -> io::Result<()> {
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

/// mtime, size and inode: every atomic save is a rename, so a new inode.
type Stamp = Option<(SystemTime, u64, u64)>;

fn stamp(path: &Path) -> io::Result<Stamp> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some((m.modified()?, m.len(), m.ino()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// One JSON document on disk with an in-memory copy that follows the file.
pub struct JsonFile<T> {
    dir: PathBuf,
    name: &'static str,
    lock_name: &'static str,
    cache: Mutex<(Stamp, T)>,
}

impl<T: Serialize + DeserializeOwned + Default + Clone> JsonFile<T> {
    pub fn new(dir: impl Into<PathBuf>, name: &'static str, lock_name: &'static str) -> Self {
        Self {
            dir: dir.into(),
            name,
            lock_name,
            cache: Mutex::new((None, T::default())),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(self.name)
    }

    fn load_file(&self) -> io::Result<T> {
        match fs::read(self.path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(e),
        }
    }

    /// Run `f` on the current contents (reloaded if the file changed).
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> io::Result<R> {
        let now = stamp(&self.path())?;
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.0 != now {
            cache.1 = self.load_file()?;
            cache.0 = now;
        }
        Ok(f(&cache.1))
    }

    /// Locked read-modify-write. `f` returns whether to save and a result.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> (bool, R)) -> io::Result<R> {
        let _lock = lock_file(&self.dir, self.lock_name)?;
        let mut value = self.load_file()?;
        let (changed, out) = f(&mut value);
        if changed {
            save_atomic(&self.dir, self.name, &value)?;
        }
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.0 = stamp(&self.path())?;
        cache.1 = value;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn json_file_roundtrip_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let a: JsonFile<Vec<String>> = JsonFile::new(&data, "x.json", "x.lock");
        assert!(a.read(|v| v.is_empty()).unwrap());
        a.update(|v| {
            v.push("one".into());
            (true, ())
        })
        .unwrap();
        // A second handle (another process, e.g. the CLI) sees the write.
        let b: JsonFile<Vec<String>> = JsonFile::new(&data, "x.json", "x.lock");
        assert_eq!(b.read(|v| v.clone()).unwrap(), vec!["one".to_string()]);
        b.update(|v| {
            v.push("two".into());
            (true, ())
        })
        .unwrap();
        assert_eq!(a.read(|v| v.len()).unwrap(), 2);
        let mode = fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mode = fs::metadata(&data).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn pkce_s256_vector() {
        // RFC 7636 appendix B.
        assert_eq!(
            sha256_b64url("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
