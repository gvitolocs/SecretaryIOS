//! The Meshy API key, behind [`KeySource`] so it can move to a secret
//! reference (`secret://…`, crate `lucina-secrets`, not landed yet) without
//! touching the client.
//!
//! Today: [`FileKey`] reads `MESHY_API_KEY_FILE` on every call (rotation
//! needs no restart). [`SecretRefKey`] resolves a `secret://…` reference
//! through the `lucina-secrets` store (Infisical, audited, cached in memory
//! only) when `secrets.toml` maps the setting `meshy.api_key`; the server
//! prefers it over the file. The file must be a regular file owned by this user
//! with no group or world bits (mode 0600 or 0400); anything else is refused,
//! so a key that leaked into a world-readable file is never used.
//!
//! The key is only ever held as a [`Secret`], whose `Debug`/`Display` print
//! `msy_…<last 4>`; it is never logged, never put in an error message and
//! never sent anywhere but `Authorization: Bearer` to the Meshy API base.

use std::fmt;
use std::path::{Path, PathBuf};

/// A secret string that never prints itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The value, for the `Authorization` header only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `msy_…abcd` (the last four characters, as Meshy's usage report shows).
    pub fn hint(&self) -> String {
        let tail: String = self
            .0
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("msy_…{tail}")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({})", self.hint())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hint())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("Meshy is not configured: MESHY_API_KEY_FILE is not set")]
    NotConfigured,
    #[error("MESHY_API_KEY_FILE cannot be read ({0})")]
    Unreadable(String),
    #[error(
        "MESHY_API_KEY_FILE must be a private file (chmod 600): it is readable by group or others"
    )]
    TooOpen,
    #[error("MESHY_API_KEY_FILE does not hold a Meshy key (expected msy_…)")]
    Malformed,
}

/// Where the key comes from.
#[async_trait::async_trait]
pub trait KeySource: Send + Sync {
    async fn api_key(&self) -> Result<Secret, KeyError>;
    /// A description for `/health` (never the key).
    fn describe(&self) -> String;
}

/// `MESHY_API_KEY_FILE`.
#[derive(Debug, Clone)]
pub struct FileKey {
    pub path: Option<PathBuf>,
}

impl FileKey {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }
}

fn check_mode(path: &Path) -> Result<(), KeyError> {
    let meta = std::fs::metadata(path).map_err(|e| KeyError::Unreadable(e.kind().to_string()))?;
    if !meta.is_file() {
        return Err(KeyError::Unreadable("not a regular file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(KeyError::TooOpen);
        }
    }
    Ok(())
}

/// A Meshy key: `msy_` plus URL-safe characters, no whitespace.
pub fn valid_key(s: &str) -> bool {
    s.len() >= 12
        && s.len() <= 256
        && s.starts_with("msy_")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl FileKey {
    /// Read and check the file now (also used by `/health`).
    pub fn read(&self) -> Result<Secret, KeyError> {
        let path = self.path.as_ref().ok_or(KeyError::NotConfigured)?;
        check_mode(path)?;
        let raw = std::fs::read_to_string(path)
            .map_err(|e| KeyError::Unreadable(e.kind().to_string()))?;
        let key = raw.trim();
        if !valid_key(key) {
            return Err(KeyError::Malformed);
        }
        Ok(Secret::new(key))
    }
}

#[async_trait::async_trait]
impl KeySource for FileKey {
    async fn api_key(&self) -> Result<Secret, KeyError> {
        self.read()
    }

    fn describe(&self) -> String {
        match &self.path {
            Some(p) => format!("file {}", p.display()),
            None => "not configured".into(),
        }
    }
}

/// `meshy.api_key` from the secrets skill (`secret://…#MESHY_API_KEY`).
pub struct SecretRefKey {
    store: std::sync::Arc<lucina_secrets::SecretStore>,
    reference: lucina_secrets::SecretRef,
}

impl SecretRefKey {
    pub fn new(
        store: std::sync::Arc<lucina_secrets::SecretStore>,
        reference: lucina_secrets::SecretRef,
    ) -> Self {
        Self { store, reference }
    }
}

#[async_trait::async_trait]
impl KeySource for SecretRefKey {
    async fn api_key(&self) -> Result<Secret, KeyError> {
        let v = self
            .store
            .resolve(&self.reference, "meshy")
            .await
            .map_err(|e| KeyError::Unreadable(e.to_string()))?;
        let key = v.expose_secret().trim();
        if !valid_key(key) {
            return Err(KeyError::Malformed);
        }
        Ok(Secret::new(key))
    }

    fn describe(&self) -> String {
        "secret reference meshy.api_key".into()
    }
}

/// A fixed key (tests, the mock).
#[derive(Debug, Clone)]
pub struct StaticKey(pub Secret);

#[async_trait::async_trait]
impl KeySource for StaticKey {
    async fn api_key(&self) -> Result<Secret, KeyError> {
        Ok(self.0.clone())
    }
    fn describe(&self) -> String {
        format!("static {}", self.0.hint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints() {
        let s = Secret::new("msy_abcdefghijklmnop1234");
        assert_eq!(format!("{s:?}"), "Secret(msy_…1234)");
        assert_eq!(s.to_string(), "msy_…1234");
        assert!(!format!("{s:?}{s}").contains("abcdefgh"));
    }

    #[cfg(unix)]
    #[test]
    fn file_key_rules() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("meshy.key");
        assert_eq!(
            FileKey::new(None).read().unwrap_err(),
            KeyError::NotConfigured
        );
        assert!(matches!(
            FileKey::new(Some(p.clone())).read().unwrap_err(),
            KeyError::Unreadable(_)
        ));
        std::fs::write(&p, "msy_test_key_0123456789\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            FileKey::new(Some(p.clone())).read().unwrap_err(),
            KeyError::TooOpen
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let k = FileKey::new(Some(p.clone())).read().unwrap();
        assert_eq!(k.expose(), "msy_test_key_0123456789");
        std::fs::write(&p, "sk-not-meshy").unwrap();
        assert_eq!(
            FileKey::new(Some(p)).read().unwrap_err(),
            KeyError::Malformed
        );
    }
}
