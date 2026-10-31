//! Secret references: `secret://<project>/<env>/<path>#<KEY>`.
//!
//! - `project`: an alias from the config's `[projects]` table (mapped to the
//!   Infisical project id), e.g. `lucina`.
//! - `env`: the Infisical environment slug, e.g. `prod`.
//! - `path`: the folder, zero or more segments, e.g. `lucina` or
//!   `lucina/github` (`/lucina/github` in Infisical).
//! - `KEY`: the secret's name in that folder.
//!
//! `secret://lucina/prod/lucina#OPEN_METEO_API_KEY` is the key
//! `OPEN_METEO_API_KEY` in folder `/lucina` of environment `prod`.

use crate::SecretError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretRef {
    pub project: String,
    pub environment: String,
    /// Always starts with `/`, no trailing slash (except the root `/`).
    pub path: String,
    pub key: String,
}

fn slug_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn segment_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn key_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl SecretRef {
    pub const SCHEME: &'static str = "secret://";

    pub fn parse(s: &str) -> Result<Self, SecretError> {
        let bad = |why: &str| {
            SecretError::BadRef(format!(
                "{why} (expected secret://<project>/<env>/<path>#<KEY>)"
            ))
        };
        let rest = s
            .trim()
            .strip_prefix(Self::SCHEME)
            .ok_or_else(|| bad("missing secret:// scheme"))?;
        let (location, key) = rest.split_once('#').ok_or_else(|| bad("missing #KEY"))?;
        if !key_ok(key) {
            return Err(bad("bad key name"));
        }
        let mut parts = location.split('/');
        let project = parts.next().unwrap_or("");
        let environment = parts.next().unwrap_or("");
        if !slug_ok(project) || !slug_ok(environment) {
            return Err(bad("bad project or environment"));
        }
        let segments: Vec<&str> = parts.filter(|p| !p.is_empty()).collect();
        if !segments.iter().all(|s| segment_ok(s)) {
            return Err(bad("bad folder path"));
        }
        Ok(Self {
            project: project.to_owned(),
            environment: environment.to_owned(),
            path: format!("/{}", segments.join("/")),
            key: key.to_owned(),
        })
    }

    /// The folder (`/lucina`) contains this ref's folder (segment-aware).
    pub fn in_folder(&self, folder: &str) -> bool {
        let f = folder.trim_end_matches('/');
        f.is_empty() || self.path == f || self.path.starts_with(&format!("{f}/"))
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = self.path.trim_start_matches('/');
        if path.is_empty() {
            write!(
                f,
                "secret://{}/{}#{}",
                self.project, self.environment, self.key
            )
        } else {
            write!(
                f,
                "secret://{}/{}/{}#{}",
                self.project, self.environment, path, self.key
            )
        }
    }
}

impl std::str::FromStr for SecretRef {
    type Err = SecretError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        let r = SecretRef::parse("secret://lucina/prod/lucina/github#GITHUB_TOKEN").unwrap();
        assert_eq!(r.project, "lucina");
        assert_eq!(r.environment, "prod");
        assert_eq!(r.path, "/lucina/github");
        assert_eq!(r.key, "GITHUB_TOKEN");
        assert_eq!(
            r.to_string(),
            "secret://lucina/prod/lucina/github#GITHUB_TOKEN"
        );
        let root = SecretRef::parse("secret://lucina/dev#X_KEY").unwrap();
        assert_eq!(root.path, "/");
        assert_eq!(root.to_string(), "secret://lucina/dev#X_KEY");
        assert!(r.in_folder("/lucina"));
        assert!(r.in_folder("/lucina/"));
        assert!(!r.in_folder("/luc"));
        assert!(!root.in_folder("/lucina"));
    }

    #[test]
    fn rejects_bad_refs() {
        for bad in [
            "lucina/prod/x#K",
            "secret://lucina/prod/x",
            "secret://lucina#K",
            "secret://lucina/prod/../etc#K",
            "secret://lu cina/prod/x#K",
            "secret://lucina/prod/x#K EY",
            "secret://lucina/prod/x#",
        ] {
            assert!(SecretRef::parse(bad).is_err(), "{bad}");
        }
    }
}
