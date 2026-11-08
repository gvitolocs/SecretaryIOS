//! `secrets.toml` (`LUCINA_SECRETS_CONFIG`). No values in it, ever: only
//! where Infisical is, which folder Lucina may read, the policy, and which
//! secret each tool uses (by reference).
//!
//! ```toml
//! [infisical]
//! url = "https://secrets.pokoin.com"
//! api = "v4"                       # "v3" for older servers
//! # organization_slug = "pokoin"
//! # credentials_dir: default $CREDENTIALS_DIRECTORY (systemd)
//! client_id_credential = "lucina-infisical-client-id"
//! client_secret_credential = "lucina-infisical-client-secret"
//!
//! [projects]                       # alias in refs -> Infisical project id
//! lucina = "00000000-0000-0000-0000-000000000000"
//!
//! [scope]                          # the one folder Lucina may read
//! project = "lucina"
//! environment = "prod"
//! path = "/lucina"
//!
//! [cache]
//! ttl_secs = 600
//!
//! [policy]
//! min_value_len = 8
//! # deny = ["password", "pin", ...]   # replaces the default deny terms
//! extra_deny = []
//! allow_names = []
//!
//! [audit]
//! path = "/home/nez/.local/state/lucina-rs/secrets-audit.jsonl"
//!
//! [redaction]
//! preload = true                   # learn every value in the folder at start
//!
//! [refs]                           # tool settings -> secret refs
//! "weather.api_key" = "secret://lucina/prod/lucina#OPEN_METEO_API_KEY"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::policy::{Policy, Scope};
use crate::{SecretError, SecretRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApiVersion {
    #[default]
    V4,
    V3,
}

fn default_id_cred() -> String {
    "lucina-infisical-client-id".into()
}
fn default_secret_cred() -> String {
    "lucina-infisical-client-secret".into()
}
fn default_timeout() -> u64 {
    10
}
fn default_ttl() -> u64 {
    600
}
fn default_min_len() -> usize {
    8
}
fn default_keep() -> usize {
    crate::audit::DEFAULT_KEEP
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfisicalConfig {
    pub url: String,
    #[serde(default)]
    pub api: ApiVersion,
    #[serde(default)]
    pub organization_slug: Option<String>,
    /// Where the systemd credentials are (default `$CREDENTIALS_DIRECTORY`).
    #[serde(default)]
    pub credentials_dir: Option<PathBuf>,
    #[serde(default = "default_id_cred")]
    pub client_id_credential: String,
    #[serde(default = "default_secret_cred")]
    pub client_secret_credential: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfig {
    pub project: String,
    pub environment: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl_secs: default_ttl(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default)]
    pub deny: Option<Vec<String>>,
    #[serde(default)]
    pub extra_deny: Vec<String>,
    #[serde(default)]
    pub allow_names: Vec<String>,
    #[serde(default = "default_min_len")]
    pub min_value_len: usize,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            deny: None,
            extra_deny: Vec::new(),
            allow_names: Vec::new(),
            min_value_len: default_min_len(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default = "default_keep")]
    pub keep: usize,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            path: None,
            keep: default_keep(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionConfig {
    #[serde(default = "yes")]
    pub preload: bool,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self { preload: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    pub infisical: InfisicalConfig,
    pub scope: ScopeConfig,
    #[serde(default)]
    pub projects: BTreeMap<String, String>,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub redaction: RedactionConfig,
    #[serde(default)]
    pub refs: BTreeMap<String, String>,
}

impl SecretsConfig {
    pub fn from_toml(text: &str) -> Result<Self, SecretError> {
        let cfg: Self = toml::from_str(text).map_err(|e| SecretError::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self, SecretError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| SecretError::Config(format!("{}: {}", path.display(), e.kind())))?;
        Self::from_toml(&text)
    }

    fn validate(&self) -> Result<(), SecretError> {
        let url = self.infisical.url.trim();
        let local = url.starts_with("http://127.0.0.1")
            || url.starts_with("http://localhost")
            || url.starts_with("http://[::1]");
        if !url.starts_with("https://") && !local {
            return Err(SecretError::Config(
                "infisical.url must be https (http only for localhost)".into(),
            ));
        }
        if !self.scope.path.starts_with('/') {
            return Err(SecretError::Config("scope.path must start with /".into()));
        }
        if !self.projects.contains_key(&self.scope.project) {
            return Err(SecretError::Config(format!(
                "scope.project \"{}\" has no entry in [projects]",
                self.scope.project
            )));
        }
        let policy = self.policy();
        for (setting, r) in &self.refs {
            let parsed = SecretRef::parse(r)?;
            policy
                .check(&parsed)
                .map_err(|e| SecretError::Config(format!("refs.\"{setting}\": {e}")))?;
        }
        Ok(())
    }

    pub fn policy(&self) -> Policy {
        let mut p = Policy::new(Scope {
            project: self.scope.project.clone(),
            environment: self.scope.environment.clone(),
            path: self.scope.path.trim_end_matches('/').to_owned(),
        });
        if let Some(d) = &self.policy.deny {
            p.deny = d.iter().map(|s| s.to_lowercase()).collect();
        }
        p.deny
            .extend(self.policy.extra_deny.iter().map(|s| s.to_lowercase()));
        p.allow_names = self.policy.allow_names.clone();
        p.min_value_len = self
            .policy
            .min_value_len
            .max(crate::redact::MIN_PATTERN_LEN);
        p
    }

    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.cache.ttl_secs.clamp(1, 3600))
    }

    /// The ref configured for a tool setting (`weather.api_key`).
    pub fn reference(&self, setting: &str) -> Option<SecretRef> {
        self.refs
            .get(setting)
            .and_then(|r| SecretRef::parse(r).ok())
    }

    pub fn credentials_dir(&self) -> Option<PathBuf> {
        self.infisical
            .credentials_dir
            .clone()
            .or_else(|| std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
[infisical]
url = "https://secrets.pokoin.com"

[projects]
lucina = "p-123"

[scope]
project = "lucina"
environment = "prod"
path = "/lucina"

[refs]
"weather.api_key" = "secret://lucina/prod/lucina#OPEN_METEO_API_KEY"
"#;

    #[test]
    fn parses_with_defaults() {
        let c = SecretsConfig::from_toml(OK).unwrap();
        assert_eq!(c.infisical.api, ApiVersion::V4);
        assert_eq!(c.ttl(), Duration::from_secs(600));
        assert_eq!(
            c.infisical.client_id_credential,
            "lucina-infisical-client-id"
        );
        assert_eq!(
            c.reference("weather.api_key").unwrap().key,
            "OPEN_METEO_API_KEY"
        );
        assert!(c.redaction.preload);
        assert_eq!(c.policy().min_value_len, 8);
    }

    #[test]
    fn the_deploy_example_parses() {
        let text = include_str!("../../../deploy/lucina-secrets/secrets.example.toml");
        let c = SecretsConfig::from_toml(text).unwrap();
        assert_eq!(c.scope.path, "/lucina");
        assert!(c.redaction.preload);
        let with_ref = text.replace("# \"weather.api_key\"", "\"weather.api_key\"");
        assert!(SecretsConfig::from_toml(&with_ref)
            .unwrap()
            .reference("weather.api_key")
            .is_some());
    }

    #[test]
    fn refuses_bad_configs() {
        let http = OK.replace("https://secrets.pokoin.com", "http://secrets.pokoin.com");
        assert!(SecretsConfig::from_toml(&http).is_err());
        let out = OK.replace("lucina/prod/lucina#OPEN", "lucina/prod/other#OPEN");
        assert!(SecretsConfig::from_toml(&out).is_err());
        let sudo = OK.replace("OPEN_METEO_API_KEY", "SUDO_PASSWORD");
        assert!(SecretsConfig::from_toml(&sudo).is_err());
        let value = format!("{OK}\nclient_secret = \"x\"\n");
        assert!(SecretsConfig::from_toml(&value).is_err());
    }
}
