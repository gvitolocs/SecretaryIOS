//! The secrets skill in the server: the model sink, the bootstrap
//! credentials the redactor must know, the Infisical store, and the
//! deterministic secrets turns (reveal refusal, status, audit).
//!
//! Sinks and where they are redacted:
//!
//! | sink | where |
//! |---|---|
//! | model prompts and replies (DeepSeek) | [`RedactingModel`] wraps every `ChatModel` |
//! | tool results | `lucina_tools::agent::execute` |
//! | API replies (app, web, Lucina MCP) | `chat::handle_chat` output |
//! | timeline (file and live stream) | `TimelineStore::append` |
//! | recent turns, job results | `RecentChat::append`, `JobStore` |
//! | Honcho writes and queries | `HonchoClient::post`, context query |
//! | logs (journald) | `lucina_secrets::log::RedactingMakeWriter` in `main` |
//! | app notifications | the `notify` tool |
//!
//! Telegram and Discord are not part of the Rust Lucina yet (Node bridge:
//! `LUCINA_REDACT=true` in the Node Lucina).

use std::path::Path;
use std::sync::Arc;

use lucina_llm::{ChatModel, ChatRequest, ChatResponse, LlmError, Message};
use lucina_secrets::reveal::{self, Ask};
use lucina_secrets::{redactor, SecretStore, SecretsConfig};

use crate::Config;

/// Every prompt to the model and every reply from it goes through the
/// redactor (message text, reasoning, tool-call arguments).
pub struct RedactingModel(pub Arc<dyn ChatModel>);

fn scrub_message(m: &mut Message) {
    let r = redactor();
    for t in [m.content.as_mut(), m.reasoning_content.as_mut()]
        .into_iter()
        .flatten()
    {
        r.redact_string(t);
    }
    for c in m.tool_calls.iter_mut().flatten() {
        r.redact_string(&mut c.function.arguments);
    }
}

#[async_trait::async_trait]
impl ChatModel for RedactingModel {
    async fn complete(&self, mut request: ChatRequest) -> Result<ChatResponse, LlmError> {
        for m in &mut request.messages {
            scrub_message(m);
        }
        let mut response = self.0.complete(request).await?;
        scrub_message(&mut response.message);
        Ok(response)
    }
}

fn file_value(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Teach the redactor the credentials Lucina starts with (values are not
/// kept, only digests). Returns how many were registered.
pub fn register_bootstrap(cfg: &Config) -> usize {
    let r = redactor();
    let mut n = 0;
    let mut add = |name: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| v.len() >= 8) {
            if r.register(name, &v) > 0 {
                n += 1;
            }
        }
    };
    add("SECRETARY_API_TOKEN", cfg.token.clone());
    add("LUCINA_NODE_TOKEN", cfg.node_token.clone());
    add(
        "DEEPSEEK_API_KEY",
        match &cfg.deepseek.key {
            lucina_llm::KeySource::Inline(k) => Some(k.clone()),
            lucina_llm::KeySource::File(p) => file_value(p),
            lucina_llm::KeySource::None => None,
        },
    );
    add("HONCHO_API_KEY", Some(cfg.honcho.api_key.clone()));
    add(
        "SECRETARY_SERVICE_TOKEN",
        cfg.secretary_service_token_file
            .as_deref()
            .and_then(file_value),
    );
    add("DSH_TOKEN", cfg.dsh.token.clone());
    add("DSH_TOKEN", file_value(&cfg.dsh.token_file));
    add("SALLING_API_KEY", cfg.deals.salling_api_key.clone());
    // postgres://user:password@host/db
    add(
        "GROCERY_DATABASE_PASSWORD",
        cfg.grocery_database_url.as_deref().and_then(|u| {
            let rest = u.split_once("://")?.1;
            let userinfo = rest.split_once('@')?.0;
            Some(userinfo.split_once(':')?.1.to_owned())
        }),
    );
    n
}

/// The Infisical store, when `LUCINA_SECRETS_CONFIG` is set. Errors are
/// logged and leave secrets off (tools then report "not configured").
pub fn open_store(cfg: &Config) -> Option<Arc<SecretStore>> {
    let path = cfg.secrets_config.as_ref()?;
    let store = SecretsConfig::load(path).and_then(SecretStore::from_config);
    match store {
        Ok(s) => {
            tracing::info!(config = %path.display(), scope = ?s.config().scope, "secrets: Infisical store ready");
            Some(Arc::new(s))
        }
        Err(e) => {
            tracing::error!(config = %path.display(), error = %e, "secrets: store not available; tools that need a secret will fail");
            None
        }
    }
}

/// A secrets turn decided without the model: the fixed refusal for reveal
/// requests (also on MCP), status and audit for the owner, a refusal for
/// MCP callers asking about secrets at all. `None`: not about secrets.
/// Returns `(reply, action)`.
pub async fn secrets_turn(
    store: Option<&SecretStore>,
    text: &str,
    mcp: bool,
) -> Option<(String, &'static str)> {
    let ask = reveal::classify(text)?;
    let lang = reveal::detect_lang(text);
    if ask == Ask::Reveal {
        tracing::warn!(mcp, "secret reveal request refused");
        return Some((reveal::refusal(lang).to_owned(), "secrets_refused"));
    }
    if mcp {
        tracing::warn!("secrets question over MCP refused");
        return Some((reveal::mcp_refusal(lang).to_owned(), "secrets_refused"));
    }
    let Some(store) = store else {
        return Some((
            lucina_secrets::status::not_configured(lang).to_owned(),
            "secrets_status",
        ));
    };
    let scope = &store.config().scope;
    let folder = format!("{}/{}{}", scope.project, scope.environment, scope.path);
    Some(match ask {
        Ask::Audit => (
            lucina_secrets::status::audit_text(lang, &store.audit_entries(15, None, None)),
            "secrets_audit",
        ),
        _ => match store.status().await {
            Ok(items) => (
                lucina_secrets::status::status_text(lang, &folder, &items),
                "secrets_status",
            ),
            Err(e) => (format!("Infisical: {e}"), "secrets_status"),
        },
    })
}
