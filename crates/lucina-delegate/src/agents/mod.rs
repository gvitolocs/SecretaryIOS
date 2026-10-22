//! The agents Lucina delegates to, behind one trait (tests use mocks).

pub mod chatgpt;
pub mod claude;
pub mod grok;
pub mod responses;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{DelegateConfig, Isolation};
use crate::types::{AgentId, Kind};

/// One delegated call.
#[derive(Debug, Clone)]
pub struct AgentJob {
    pub job_id: String,
    pub kind: Kind,
    /// Sanitised (no secrets, no secret refs).
    pub prompt: String,
    /// Where the agent starts.
    pub workdir: PathBuf,
    /// What the sandbox may write (the job directory).
    pub writable: PathBuf,
    pub timeout: Duration,
    /// Claude: `--max-turns` (budget left, per-job cap).
    pub max_turns: u32,
    /// Claude: continue this session.
    pub resume_session: Option<String>,
    pub isolation: Isolation,
}

/// What came back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentReply {
    pub text: String,
    pub session: Option<String>,
    pub turns: u32,
    /// The path used: `claude-cli`, `gbot`, `xai-api`, `codex-cli`, `openai-api`.
    pub backend: String,
}

#[async_trait::async_trait]
pub trait Agent: Send + Sync {
    fn id(&self) -> AgentId;
    fn supports(&self, kind: Kind) -> bool;
    /// The backend this agent would use now, or why it cannot run.
    async fn backend(&self) -> Result<String, String>;
    async fn run(&self, job: &AgentJob) -> Result<AgentReply, String>;
}

/// The real agents from the config.
pub fn real(
    cfg: &DelegateConfig,
    http: reqwest::Client,
    secrets: Option<Arc<lucina_secrets::store::SecretStore>>,
) -> Vec<Arc<dyn Agent>> {
    vec![
        Arc::new(claude::ClaudeCli::new(cfg.claude.clone())),
        Arc::new(grok::Grok::new(
            cfg.grok.clone(),
            http.clone(),
            secrets.clone(),
        )),
        Arc::new(chatgpt::Chatgpt::new(cfg.chatgpt.clone(), http, secrets)),
    ]
}

/// A key from the secrets skill (setting name) or a key file.
pub(crate) async fn api_key(
    secrets: Option<&lucina_secrets::store::SecretStore>,
    setting: &str,
    file: Option<&PathBuf>,
    tool: &str,
) -> Option<lucina_secrets::SecretValue> {
    if let Some(store) = secrets {
        match store.resolve_setting(setting, tool).await {
            Ok(Some(v)) if !v.is_empty() => return Some(v),
            Ok(_) => {}
            Err(e) => tracing::warn!(%setting, error = %e, "delegate: api key not resolved"),
        }
    }
    let text = std::fs::read_to_string(file?).ok()?;
    let key = text.trim().to_owned();
    (!key.is_empty()).then(|| {
        // Known from now on: the redactor masks it in every sink.
        lucina_secrets::redactor().register(tool, &key);
        lucina_secrets::SecretValue::new(key)
    })
}

/// Whether a key is configured (without resolving its value when possible).
pub(crate) fn has_key(
    secrets: Option<&lucina_secrets::store::SecretStore>,
    setting: &str,
    file: Option<&PathBuf>,
) -> bool {
    secrets.is_some_and(|s| s.reference(setting).is_some()) || file.is_some_and(|f| f.is_file())
}
