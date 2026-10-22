//! [`SecretStore`]: the one way tools get a secret.
//!
//! `resolve(ref, tool)`: policy (scope, hard deny, deny terms) -> TTL cache
//! -> Infisical -> minimum length -> register with the redactor -> audit ->
//! a [`SecretValue`] for the outbound call. Every path is audited, never
//! with the value.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

use crate::audit::{AuditEntry, AuditLog};
use crate::cache::TtlCache;
use crate::config::SecretsConfig;
use crate::infisical::{Credentials, InfisicalClient, SecretMeta};
use crate::policy::{hard_denied, Policy};
use crate::{redactor, SecretError, SecretRef, SecretValue};

struct Cached {
    value: SecretValue,
}

impl zeroize::Zeroize for Cached {
    fn zeroize(&mut self) {
        self.value.zeroize();
    }
}

/// One line of `secrets_status` (no value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    /// Present in the folder with a value.
    pub set: bool,
    /// Tool settings that use it (`weather.api_key`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// Last change in Infisical (rotation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_by: Option<String>,
    /// Lucina may not resolve it (policy), with the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

pub struct SecretStore {
    cfg: SecretsConfig,
    policy: Policy,
    project_id: String,
    client: InfisicalClient,
    cache: Mutex<TtlCache<SecretRef, Cached>>,
    audit: AuditLog,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore")
            .field("scope", &self.policy.scope)
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl SecretStore {
    /// Credentials from the systemd credentials directory.
    pub fn from_config(cfg: SecretsConfig) -> Result<Self, SecretError> {
        let dir = cfg.credentials_dir().ok_or_else(|| {
            SecretError::Config(
                "no credentials directory (CREDENTIALS_DIRECTORY unset and infisical.credentials_dir missing)".into(),
            )
        })?;
        let creds = Credentials::from_dir(
            &dir,
            &cfg.infisical.client_id_credential,
            &cfg.infisical.client_secret_credential,
        )?;
        Self::with_credentials(cfg, creds)
    }

    pub fn with_credentials(cfg: SecretsConfig, creds: Credentials) -> Result<Self, SecretError> {
        let policy = cfg.policy();
        let project_id = cfg
            .projects
            .get(&cfg.scope.project)
            .cloned()
            .ok_or_else(|| SecretError::Config("scope project has no id".into()))?;
        let client = InfisicalClient::new(&cfg.infisical, creds)?;
        let audit = AuditLog::new(cfg.audit.path.clone(), cfg.audit.keep);
        Ok(Self {
            cache: Mutex::new(TtlCache::new(cfg.ttl())),
            policy,
            project_id,
            client,
            audit,
            cfg,
        })
    }

    pub fn config(&self) -> &SecretsConfig {
        &self.cfg
    }

    pub fn audit_path(&self) -> Option<PathBuf> {
        self.audit.path().map(PathBuf::from)
    }

    fn audit(&self, r: &SecretRef, tool: &str, res: Result<bool, &SecretError>) {
        let (ok, cached, error) = match res {
            Ok(cached) => (true, cached, None),
            Err(e) => (false, false, Some(e.code().to_owned())),
        };
        self.audit.record(AuditEntry {
            ts: now_rfc3339(),
            reference: r.to_string(),
            name: r.key.clone(),
            tool: tool.to_owned(),
            ok,
            cached,
            error,
        });
        tracing::info!(secret = %r.key, %tool, ok, cached, "secret resolved");
    }

    /// The ref configured for a tool setting, if any.
    pub fn reference(&self, setting: &str) -> Option<SecretRef> {
        self.cfg.reference(setting)
    }

    /// Resolve the secret behind a tool setting (`weather.api_key`).
    /// `Ok(None)`: no ref configured for it.
    pub async fn resolve_setting(
        &self,
        setting: &str,
        tool: &str,
    ) -> Result<Option<SecretValue>, SecretError> {
        match self.reference(setting) {
            Some(r) => self.resolve(&r, tool).await.map(Some),
            None => Ok(None),
        }
    }

    /// The value behind `r` for `tool`'s outbound call.
    pub async fn resolve(&self, r: &SecretRef, tool: &str) -> Result<SecretValue, SecretError> {
        let res = self.resolve_inner(r).await;
        match &res {
            Ok((_, cached)) => self.audit(r, tool, Ok(*cached)),
            Err(e) => {
                self.audit(r, tool, Err(e));
                tracing::warn!(secret = %r.key, %tool, error = %e, "secret not resolved");
            }
        }
        res.map(|(v, _)| v)
    }

    async fn resolve_inner(&self, r: &SecretRef) -> Result<(SecretValue, bool), SecretError> {
        self.policy.check(r)?;
        {
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            c.purge_expired(now);
            if let Some(hit) = c.get(r, now) {
                return Ok((hit.value.duplicate(), true));
            }
        }
        let fetched = self
            .client
            .get_secret(&self.project_id, &r.environment, &r.path, &r.key)
            .await?;
        // Learn it before anything else can echo it.
        redactor().register(&r.key, fetched.value.expose_secret());
        if fetched.value.len() < self.policy.min_value_len {
            return Err(SecretError::TooShort(
                r.to_string(),
                self.policy.min_value_len,
            ));
        }
        let out = fetched.value.duplicate();
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
            r.clone(),
            Cached {
                value: fetched.value,
            },
            Instant::now(),
        );
        Ok((out, false))
    }

    /// Drop every cached value (zeroized), e.g. after a rotation.
    pub fn forget_cached(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Learn every value in Lucina's folder (redactor only; nothing cached,
    /// nothing returned). Audited as `redactor-preload`.
    pub async fn preload_redaction(&self) -> Result<usize, SecretError> {
        let scope = &self.policy.scope;
        let res = self
            .client
            .for_each_value(&self.project_id, &scope.environment, &scope.path, |k, v| {
                redactor().register(k, v);
            })
            .await;
        let r = SecretRef {
            project: scope.project.clone(),
            environment: scope.environment.clone(),
            path: if scope.path.is_empty() {
                "/".into()
            } else {
                scope.path.clone()
            },
            key: "*".into(),
        };
        self.audit(&r, "redactor-preload", res.as_ref().map(|_| false));
        res
    }

    /// Names in the folder, set or missing, rotation and last use. Refs
    /// configured for tools but missing in Infisical show as not set.
    pub async fn status(&self) -> Result<Vec<SecretStatus>, SecretError> {
        let scope = &self.policy.scope;
        let listed: Vec<SecretMeta> = self
            .client
            .list(&self.project_id, &scope.environment, &scope.path)
            .await?;
        let folder = if scope.path.is_empty() {
            "/".to_owned()
        } else {
            scope.path.clone()
        };
        let mut out: Vec<SecretStatus> = Vec::new();
        let used_by = |key: &str, path: &str| -> Vec<String> {
            self.cfg
                .refs
                .iter()
                .filter(|(_, r)| SecretRef::parse(r).is_ok_and(|p| p.key == key && p.path == path))
                .map(|(s, _)| s.clone())
                .collect()
        };
        for m in &listed {
            let r = SecretRef {
                project: scope.project.clone(),
                environment: scope.environment.clone(),
                path: m.path.clone().unwrap_or_else(|| folder.clone()),
                key: m.key.clone(),
            };
            let users = used_by(&r.key, &r.path);
            out.push(self.status_line(&r, true, m.version, m.updated_at.clone(), users));
        }
        // Refs configured for tools but not in Infisical: missing.
        for (setting, raw) in &self.cfg.refs {
            let Ok(r) = SecretRef::parse(raw) else {
                continue;
            };
            if out.iter().any(|s| s.reference == r.to_string()) {
                continue;
            }
            out.push(self.status_line(&r, false, None, None, vec![setting.clone()]));
        }
        out.sort_by(|a, b| a.reference.cmp(&b.reference));
        Ok(out)
    }

    fn status_line(
        &self,
        r: &SecretRef,
        set: bool,
        version: Option<u64>,
        last_rotated: Option<String>,
        used_by: Vec<String>,
    ) -> SecretStatus {
        let last = self.audit.last_use(&r.key);
        let refused = self.policy.check(r).err().map(|e| match e {
            SecretError::Denied(_, why) => why,
            other => other.to_string(),
        });
        let refused = refused.or_else(|| {
            hard_denied(&r.key, &r.path)
                .map(|t| format!("looks like a login, sudo or root password (\"{t}\")"))
        });
        SecretStatus {
            name: r.key.clone(),
            reference: r.to_string(),
            set,
            used_by,
            version,
            last_rotated,
            last_used: last.as_ref().map(|e| e.ts.clone()),
            last_used_by: last.map(|e| e.tool),
            refused,
        }
    }

    /// The audit log, newest first.
    pub fn audit_entries(
        &self,
        limit: usize,
        name: Option<&str>,
        tool: Option<&str>,
    ) -> Vec<AuditEntry> {
        self.audit.entries(limit.clamp(1, 200), name, tool)
    }
}
