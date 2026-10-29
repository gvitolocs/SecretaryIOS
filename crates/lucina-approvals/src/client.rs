//! The requester side of the approvals API (contract v1): create and wait.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ApprovalError;

#[derive(Debug, Clone, Serialize)]
pub struct RequesterInfo {
    pub kind: String,
    pub name: String,
    pub detail: String,
}

/// `POST /v1/approvals` body.
#[derive(Debug, Clone, Serialize)]
pub struct NewApproval {
    pub requester: RequesterInfo,
    pub action: String,
    /// At most 200 characters (clipped).
    pub summary: String,
    /// At most 4000 characters, exact.
    pub details: String,
    pub risk: String,
    pub sensitive: bool,
    #[serde(rename = "payloadHash")]
    pub payload_hash: String,
    #[serde(rename = "ttlSecs")]
    pub ttl_secs: u64,
}

pub const MAX_SUMMARY: usize = 200;
pub const MAX_DETAILS: usize = 4000;

/// Clip to `max` characters (with an ellipsis).
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Created {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaitReply {
    #[serde(default)]
    pub id: String,
    pub status: Status,
    #[serde(rename = "decidedBy", default)]
    pub decided_by: Option<String>,
    #[serde(default)]
    pub grant: Option<String>,
}

/// Where the service token comes from.
#[derive(Clone)]
enum TokenSource {
    /// A file, read per call (it can rotate).
    File(PathBuf),
    /// A value handed over in the environment (agentd's `LUCINA_AGENTD_NOTIFY_TOKEN`).
    Value(String),
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: TokenSource,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish()
    }
}

impl Client {
    pub fn new(base: &str, token_file: PathBuf) -> Result<Self, ApprovalError> {
        Self::build(base, TokenSource::File(token_file))
    }

    /// A client whose `svc_` token is given as a value (never printed or logged).
    pub fn with_token(base: &str, token: String) -> Result<Self, ApprovalError> {
        Self::build(base, TokenSource::Value(token))
    }

    fn build(base: &str, token: TokenSource) -> Result<Self, ApprovalError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-approvals/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| ApprovalError::Http(e.to_string()))?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            token,
        })
    }

    /// The service token (a file is read per call, it can rotate).
    fn token(&self) -> Result<String, ApprovalError> {
        let t = match &self.token {
            TokenSource::File(path) => std::fs::read_to_string(path)
                .map_err(|e| {
                    ApprovalError::NotConfigured(format!(
                        "service token file {}: {}",
                        path.display(),
                        e.kind()
                    ))
                })?
                .trim()
                .to_owned(),
            TokenSource::Value(v) => v.trim().to_owned(),
        };
        if t.starts_with("svc_") {
            Ok(t)
        } else {
            Err(ApprovalError::NotConfigured(
                "the service token file does not hold a svc_ token".into(),
            ))
        }
    }

    async fn read<T: for<'de> Deserialize<'de>>(
        resp: reqwest::Response,
    ) -> Result<T, ApprovalError> {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ApprovalError::Http(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&body)
                .map_err(|e| ApprovalError::Http(format!("unexpected answer: {e}")));
        }
        let code = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["error"]
                    .as_str()
                    .or_else(|| v["error"]["code"].as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        if status.as_u16() == 404 {
            return Err(ApprovalError::NotFound);
        }
        Err(ApprovalError::Server {
            status: status.as_u16(),
            code,
        })
    }

    /// `POST /v1/approvals`.
    pub async fn create(&self, req: &NewApproval) -> Result<Created, ApprovalError> {
        let token = self.token()?;
        let resp = self
            .http
            .post(format!("{}/v1/approvals", self.base))
            .bearer_auth(token)
            .timeout(Duration::from_secs(15))
            .json(req)
            .send()
            .await
            .map_err(|e| ApprovalError::Http(e.without_url().to_string()))?;
        Self::read(resp).await
    }

    /// `POST /v1/approvals/{id}/withdraw`: the requester no longer needs the
    /// answer. Idempotent. A pending request becomes `expired`; one that was
    /// decided keeps its status. `NotFound` means the server no longer knows it.
    pub async fn withdraw(&self, id: &str) -> Result<Status, ApprovalError> {
        #[derive(Deserialize)]
        struct Reply {
            status: Status,
        }
        let token = self.token()?;
        let resp = self
            .http
            .post(format!("{}/v1/approvals/{id}/withdraw", self.base))
            .bearer_auth(token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ApprovalError::Http(e.without_url().to_string()))?;
        Ok(Self::read::<Reply>(resp).await?.status)
    }

    /// One long poll, `GET /v1/approvals/{id}/wait?timeout=…`. `local` caps the
    /// HTTP request; hitting it counts as "still pending".
    pub async fn wait_once(
        &self,
        id: &str,
        timeout_secs: u64,
        local: Duration,
    ) -> Result<WaitReply, ApprovalError> {
        let token = self.token()?;
        let fut = self
            .http
            .get(format!("{}/v1/approvals/{id}/wait", self.base))
            .query(&[("timeout", timeout_secs.clamp(1, 25))])
            .bearer_auth(token)
            .send();
        let resp = match tokio::time::timeout(local, fut).await {
            Err(_) => {
                return Ok(WaitReply {
                    id: id.to_owned(),
                    status: Status::Pending,
                    decided_by: None,
                    grant: None,
                })
            }
            Ok(r) => r.map_err(|e| ApprovalError::Http(e.without_url().to_string()))?,
        };
        Self::read(resp).await
    }
}
