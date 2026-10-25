//! The Cloudflare Access service token. Values are never printed: only a child process's environment gets them.

use std::fmt;
use std::path::{Path, PathBuf};

pub const ID_KEY: &str = "LUCINA_LINK_CF_CLIENT_ID";
pub const SECRET_KEY: &str = "LUCINA_LINK_CF_CLIENT_SECRET";

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub client_id: Secret,
    pub client_secret: Secret,
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("{0} is readable by group or others; run chmod 600 on it")]
    TooOpen(PathBuf),
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// `~/.config/lucina-link/secrets.env`.
pub fn default_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("secrets.env"))
}

/// The token from the environment, else from `~/.config/lucina-link/secrets.env`; `None` when neither has it.
pub fn load() -> Result<Option<AccessToken>, SecretsError> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match default_path() {
        Some(p) => load_from(env, &p),
        None => load_from(env, Path::new("/nonexistent")),
    }
}

pub fn load_from(
    env: impl Fn(&str) -> Option<String>,
    path: &Path,
) -> Result<Option<AccessToken>, SecretsError> {
    if let (Some(id), Some(secret)) = (env(ID_KEY), env(SECRET_KEY)) {
        return Ok(Some(AccessToken {
            client_id: Secret(id),
            client_secret: Secret(secret),
        }));
    }
    if !path.exists() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|source| SecretsError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(SecretsError::TooOpen(path.to_path_buf()));
        }
    }
    let text = std::fs::read_to_string(path).map_err(|source| SecretsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut id = None;
    let mut secret = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        match k.trim().trim_start_matches("export ") {
            ID_KEY => id = Some(v.to_string()),
            SECRET_KEY => secret = Some(v.to_string()),
            _ => {}
        }
    }
    Ok(match (id, secret) {
        (Some(i), Some(s)) if !i.is_empty() && !s.is_empty() => Some(AccessToken {
            client_id: Secret(i),
            client_secret: Secret(s),
        }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, body: &str, mode: u32) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lucina-link-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("secrets.env");
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        p
    }

    const BODY: &str = "# token\nLUCINA_LINK_CF_CLIENT_ID=file-id.access\nLUCINA_LINK_CF_CLIENT_SECRET=file-secret\n";

    #[test]
    fn env_wins_over_file() {
        let p = temp_file("env", BODY, 0o600);
        let env = |k: &str| match k {
            ID_KEY => Some("env-id".to_string()),
            SECRET_KEY => Some("env-secret".to_string()),
            _ => None,
        };
        let t = load_from(env, &p).unwrap().unwrap();
        assert_eq!(t.client_id.expose(), "env-id");
        let t = load_from(|_| None, &p).unwrap().unwrap();
        assert_eq!(t.client_id.expose(), "file-id.access");
        assert_eq!(t.client_secret.expose(), "file-secret");
    }

    #[cfg(unix)]
    #[test]
    fn open_file_is_refused() {
        let p = temp_file("open", BODY, 0o644);
        assert!(matches!(
            load_from(|_| None, &p),
            Err(SecretsError::TooOpen(_))
        ));
    }

    #[test]
    fn missing_file_is_none() {
        assert!(load_from(|_| None, Path::new("/nonexistent/secrets.env"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn debug_and_display_redact() {
        let t = AccessToken {
            client_id: Secret::new("abc.access"),
            client_secret: Secret::new("hunter2"),
        };
        let shown = format!("{t:?} {}", t.client_secret);
        assert!(
            !shown.contains("hunter2") && !shown.contains("abc.access"),
            "{shown}"
        );
        assert!(shown.contains("redacted"));
    }
}
