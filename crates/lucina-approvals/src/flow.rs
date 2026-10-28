//! Ask the owner and wait: create (or reuse a pending request), long-poll,
//! verify the grant, spend it once.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::client::{Client, NewApproval, Status, MAX_DETAILS};
use crate::config::ApprovalsConfig;
use crate::grant::{self, Claims, Expect, GrantError, PubKey};
use crate::used::UsedRegistry;
use crate::{now_unix, ApprovalError};

/// What the owner decided (or has not yet).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Approved, the grant verified and was spent.
    Approved(Claims),
    Denied {
        id: String,
        decided_by: Option<String>,
    },
    Expired {
        id: String,
    },
    /// Still waiting when `max_wait` ran out; the request stays open and a
    /// later [`Approvals::ask`] for the same thing reuses it.
    Pending {
        id: String,
        expires_at: i64,
    },
}

/// A request that exists on the server.
#[derive(Debug, Clone)]
pub struct Begun {
    pub id: String,
    /// Unix seconds (0 when the server sent none).
    pub expires_at: i64,
    /// An earlier pending request was reused instead of creating one.
    pub reused: bool,
}

pub struct Approvals {
    cfg: ApprovalsConfig,
    client: Client,
    used: UsedRegistry,
    /// (action, payloadHash) -> (id, expires at): pending requests a
    /// follow-up for the same thing reuses.
    reuse: Mutex<HashMap<(String, String), (String, i64)>>,
    /// Plans being waited for in the background (payload hash).
    pub(crate) inflight: Mutex<std::collections::HashSet<String>>,
}

impl std::fmt::Debug for Approvals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Approvals")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

fn parse_rfc3339(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

impl Approvals {
    /// `Err(NotConfigured)` without server, token file or a readable pinned key.
    pub fn new(cfg: ApprovalsConfig) -> Result<Self, ApprovalError> {
        let (Some(url), Some(token)) = (cfg.server_url.clone(), cfg.token_file.clone()) else {
            return Err(ApprovalError::NotConfigured(
                "LUCINA_SECRETARY_SERVER_URL and LUCINA_SECRETARY_SERVICE_TOKEN_FILE".into(),
            ));
        };
        PubKey::load(&cfg.pubkey_file).map_err(ApprovalError::NotConfigured)?;
        Ok(Self {
            client: Client::new(&url, token)?,
            used: UsedRegistry::new(cfg.used_file.clone()),
            cfg,
            reuse: Mutex::new(HashMap::new()),
            inflight: Mutex::new(Default::default()),
        })
    }

    pub fn config(&self) -> &ApprovalsConfig {
        &self.cfg
    }

    /// The pinned key, read from its file on every use (so a rotation needs no restart).
    fn key(&self) -> Result<PubKey, ApprovalError> {
        PubKey::load(&self.cfg.pubkey_file).map_err(ApprovalError::NotConfigured)
    }

    fn remembered(&self, key: &(String, String)) -> Option<(String, i64)> {
        let mut m = self.reuse.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        m.retain(|_, (_, exp)| *exp > now + 2);
        m.get(key).cloned()
    }

    fn forget(&self, key: &(String, String)) {
        self.reuse
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    fn check(&self, req: &NewApproval) -> Result<(), ApprovalError> {
        if req.details.chars().count() > MAX_DETAILS {
            return Err(ApprovalError::Invalid(format!(
                "the details are longer than {MAX_DETAILS} characters, so the owner could not see them in full"
            )));
        }
        if !req.payload_hash.starts_with("sha256:") {
            return Err(ApprovalError::Invalid(
                "payloadHash must be sha256:<hex>".into(),
            ));
        }
        Ok(())
    }

    /// Reuse a still-pending request for the same (action, payloadHash), else create one.
    pub async fn begin(&self, req: &NewApproval) -> Result<Begun, ApprovalError> {
        self.check(req)?;
        self.key()?;
        let key = (req.action.clone(), req.payload_hash.clone());
        if let Some((id, exp)) = self.remembered(&key) {
            return Ok(Begun {
                id,
                expires_at: exp,
                reused: true,
            });
        }
        self.create(req).await
    }

    async fn create(&self, req: &NewApproval) -> Result<Begun, ApprovalError> {
        let key = (req.action.clone(), req.payload_hash.clone());
        let created = self.client.create(req).await?;
        let expires_at = parse_rfc3339(&created.expires_at);
        let exp = if expires_at > 0 {
            expires_at
        } else {
            now_unix() + req.ttl_secs as i64
        };
        self.reuse
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, (created.id.clone(), exp));
        Ok(Begun {
            id: created.id,
            expires_at: exp,
            reused: false,
        })
    }

    /// Wait for the owner up to `max_wait`; on approval verify the grant
    /// (pinned key, exp, action, payload hash, id, biometric) and spend it.
    pub async fn finish(
        &self,
        req: &NewApproval,
        begun: &Begun,
        max_wait: Duration,
    ) -> Result<Outcome, ApprovalError> {
        let key = (req.action.clone(), req.payload_hash.clone());
        let deadline = Instant::now() + max_wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Outcome::Pending {
                    id: begun.id.clone(),
                    expires_at: begun.expires_at,
                });
            }
            let polled = Instant::now();
            let secs = remaining.as_secs_f64().ceil() as u64;
            let reply = match self
                .client
                .wait_once(&begun.id, secs, remaining + Duration::from_millis(250))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if e == ApprovalError::NotFound {
                        self.forget(&key);
                    }
                    return Err(e);
                }
            };
            match reply.status {
                Status::Pending => {
                    if polled.elapsed() < Duration::from_millis(50) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                Status::Denied => {
                    self.forget(&key);
                    return Ok(Outcome::Denied {
                        id: begun.id.clone(),
                        decided_by: reply.decided_by,
                    });
                }
                Status::Expired => {
                    self.forget(&key);
                    return Ok(Outcome::Expired {
                        id: begun.id.clone(),
                    });
                }
                Status::Approved => {
                    let grant = reply.grant.ok_or(ApprovalError::NoGrant)?;
                    let now = now_unix();
                    let result = grant::verify(
                        &self.key()?,
                        &grant,
                        &Expect {
                            action: &req.action,
                            payload_hash: &req.payload_hash,
                            id: Some(&begun.id),
                            require_biometric: self.cfg.require_biometric && req.sensitive,
                            now,
                        },
                    )
                    .map_err(ApprovalError::from)
                    .and_then(|claims| {
                        self.used.consume(&claims.id, claims.exp, now)?;
                        Ok(claims)
                    });
                    self.forget(&key);
                    return result.map(Outcome::Approved);
                }
            }
        }
    }

    /// [`begin`](Self::begin) + [`finish`](Self::finish). A reused request that
    /// turns out spent (its grant was already used or has expired) or unknown
    /// is replaced by a fresh one, once.
    pub async fn ask(
        &self,
        req: &NewApproval,
        max_wait: Duration,
    ) -> Result<Outcome, ApprovalError> {
        let begun = self.begin(req).await?;
        let first = self.finish(req, &begun, max_wait).await;
        let stale = matches!(
            first,
            Err(ApprovalError::NotFound)
                | Err(ApprovalError::Grant(
                    GrantError::Replay | GrantError::Expired
                ))
        );
        if begun.reused && stale {
            let fresh = self.create(req).await?;
            return self.finish(req, &fresh, max_wait).await;
        }
        first
    }
}
