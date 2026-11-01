//! The secrets skill in the server: the model sink, the bootstrap
//! credentials the redactor must know, the Infisical store, and the
//! deterministic secrets turns (reveal refusal, status, audit).
//!
//! Sinks and where they are redacted:
//!
//! | sink | where |
//! |---|---|
//! | model prompts and replies (DeepSeek) | [`RedactingModel`] wraps every `ChatModel`; prompts also lose OTP codes |
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
/// redactor (message text, reasoning, tool-call arguments). Prompts also get
/// the OTP pass (`lucina_secrets::otp`): a one-time code never reaches the
/// external model, whatever brought it into the context.
pub struct RedactingModel(pub Arc<dyn ChatModel>);

fn scrub_message(m: &mut Message, outbound: bool) {
    let r = redactor();
    let scrub = |t: &mut String| {
        if outbound {
            r.redact_outbound_string(t)
        } else {
            r.redact_string(t)
        }
    };
    for t in [m.content.as_mut(), m.reasoning_content.as_mut()]
        .into_iter()
        .flatten()
    {
        scrub(t);
    }
    for c in m.tool_calls.iter_mut().flatten() {
        scrub(&mut c.function.arguments);
    }
}

#[async_trait::async_trait]
impl ChatModel for RedactingModel {
    async fn complete(&self, mut request: ChatRequest) -> Result<ChatResponse, LlmError> {
        for m in &mut request.messages {
            scrub_message(m, true);
        }
        let mut response = self.0.complete(request).await?;
        scrub_message(&mut response.message, false);
        Ok(response)
    }

    /// Streamed text is redacted a word at a time: a fragment is held back to the last whitespace, so a
    /// value split across two deltas is whole when it is checked. The assembled response is redacted as
    /// a whole, as in `complete`.
    async fn complete_streaming(
        &self,
        mut request: ChatRequest,
        on_delta: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> Result<ChatResponse, LlmError> {
        for m in &mut request.messages {
            scrub_message(m, true);
        }
        let mut held = String::new();
        let mut response = {
            let mut forward = |d: &str| {
                held.push_str(d);
                if let Some((i, c)) = held.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
                    let mut ready: String = held.drain(..i + c.len_utf8()).collect();
                    redactor().redact_string(&mut ready);
                    on_delta(&ready);
                }
            };
            self.0.complete_streaming(request, &mut forward).await?
        };
        if !held.is_empty() {
            redactor().redact_string(&mut held);
            on_delta(&held);
        }
        scrub_message(&mut response.message, false);
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

/// An MCP caller that may ask the owner for permission instead of being refused.
pub struct McpApproval<'a> {
    pub approvals: &'a lucina_approvals::Approvals,
    /// The Lucina MCP client (`claude_code`, `chatgpt`, ...).
    pub caller: &'a str,
}

fn denied_text(lang: reveal::Lang) -> &'static str {
    match lang {
        reveal::Lang::It => {
            "Giuseppe ha negato il permesso: non condivido informazioni sui segreti tramite MCP."
        }
        reveal::Lang::En => {
            "Giuseppe denied the request, so I'm not sharing information about secrets over MCP."
        }
        reveal::Lang::Da => {
            "Giuseppe afviste anmodningen, så jeg deler ikke oplysninger om hemmeligheder via MCP."
        }
    }
}

fn expired_text(lang: reveal::Lang) -> &'static str {
    match lang {
        reveal::Lang::It => "Giuseppe non ha risposto in tempo: la richiesta è scaduta e non condivido nulla sui segreti tramite MCP.",
        reveal::Lang::En => "Giuseppe didn't answer in time: the request expired and I'm not sharing anything about secrets over MCP.",
        reveal::Lang::Da => "Giuseppe svarede ikke i tide: anmodningen udløb, og jeg deler ikke noget om hemmeligheder via MCP.",
    }
}

fn pending_text(lang: reveal::Lang, id: &str) -> String {
    match lang {
        reveal::Lang::It => format!("Ho chiesto il permesso a Giuseppe (richiesta {id}) ma non ha ancora risposto. Chiedi di nuovo tra poco: se approva, ti rispondo."),
        reveal::Lang::En => format!("I asked Giuseppe for permission (request {id}) but he hasn't answered yet. Ask again in a moment: if he approves, I'll answer."),
        reveal::Lang::Da => format!("Jeg har bedt Giuseppe om tilladelse (anmodning {id}), men han har ikke svaret endnu. Spørg igen om lidt: godkender han, svarer jeg."),
    }
}

/// `Ok(())`: the owner approved (grant verified and spent). `Err`: the reply to give instead.
async fn owner_approval(
    store: &SecretStore,
    gate: &McpApproval<'_>,
    ask: Ask,
    text: &str,
    lang: reveal::Lang,
) -> Result<(), (String, &'static str)> {
    use lucina_approvals::{client::clip, NewApproval, Outcome, RequesterInfo};
    let kind = if ask == Ask::Audit { "audit" } else { "status" };
    let scope = &store.config().scope;
    let folder = format!("{}/{}{}", scope.project, scope.environment, scope.path);
    // The question is the "why"; it is redacted like every sink, one line, short.
    let mut why = text.split_whitespace().collect::<Vec<_>>().join(" ");
    redactor().redact_string(&mut why);
    let why = clip(&why, 200);
    let hash = lucina_approvals::payload_hash(&serde_json::json!({
        "action": "secret.reveal", "client": gate.caller, "folder": folder, "kind": kind,
    }));
    let cfg = gate.approvals.config();
    let req = NewApproval {
        requester: RequesterInfo {
            kind: "lucina".into(),
            name: "Lucina".into(),
            detail: clip(&format!("{} via Lucina MCP", gate.caller), 200),
        },
        action: "secret.reveal".into(),
        summary: clip(
            &format!("{} asks for Lucina's secrets {kind} (names only, no values)", gate.caller),
            200,
        ),
        details: format!(
            "client: {}\nasks: secrets {kind} (names and metadata, never a value)\nfolder: {folder}\nwhy: {why}",
            gate.caller
        ),
        risk: "medium".into(),
        sensitive: true,
        payload_hash: hash,
        ttl_secs: cfg.ttl_secs,
    };
    match gate.approvals.ask(&req, cfg.guard_wait).await {
        Ok(Outcome::Approved(claims)) => {
            tracing::info!(client = gate.caller, approval = %claims.id, "secrets over MCP: the owner approved");
            Ok(())
        }
        Ok(Outcome::Denied { id, .. }) => {
            tracing::warn!(client = gate.caller, approval = %id, "secrets over MCP: the owner denied");
            Err((denied_text(lang).to_owned(), "secrets_refused"))
        }
        Ok(Outcome::Expired { id }) => {
            tracing::warn!(client = gate.caller, approval = %id, "secrets over MCP: the request expired");
            Err((expired_text(lang).to_owned(), "secrets_refused"))
        }
        Ok(Outcome::Pending { id, .. }) => {
            tracing::info!(client = gate.caller, approval = %id, "secrets over MCP: the owner was asked, no answer yet");
            Err((pending_text(lang, &id), "secrets_approval_pending"))
        }
        Err(e) => {
            // Fail closed: the old refusal.
            tracing::warn!(client = gate.caller, error = %e, "secrets over MCP: approval unavailable, refused");
            Err((reveal::mcp_refusal(lang).to_owned(), "secrets_refused"))
        }
    }
}

/// A secrets turn decided without the model: the fixed refusal for reveal
/// requests (also on MCP: Lucina has no way to read a value), status and
/// audit for the owner. For MCP callers those two are the owner's alone
/// unless `gate` is given (owner approvals configured): Lucina then asks
/// Giuseppe (`secret.reveal`) and answers as for the owner only with a
/// verified grant; deny, expiry and any error refuse. `None`: not about
/// secrets. Returns `(reply, action)`.
pub async fn secrets_turn(
    store: Option<&SecretStore>,
    text: &str,
    mcp: bool,
    gate: Option<McpApproval<'_>>,
) -> Option<(String, &'static str)> {
    let ask = reveal::classify(text)?;
    let lang = reveal::detect_lang(text);
    if ask == Ask::Reveal {
        tracing::warn!(mcp, "secret reveal request refused");
        return Some((reveal::refusal(lang).to_owned(), "secrets_refused"));
    }
    if mcp {
        match (gate, store) {
            (Some(gate), Some(store)) => {
                if let Err(reply) = owner_approval(store, &gate, ask, text, lang).await {
                    return Some(reply);
                }
            }
            _ => {
                tracing::warn!("secrets question over MCP refused");
                return Some((reveal::mcp_refusal(lang).to_owned(), "secrets_refused"));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_llm::ScriptedModel;

    /// Streams `parts` as deltas, then answers with their concatenation.
    struct Streamer(Vec<&'static str>);

    #[async_trait::async_trait]
    impl ChatModel for Streamer {
        async fn complete(&self, _: ChatRequest) -> Result<ChatResponse, LlmError> {
            let m = ScriptedModel::new();
            m.push_text(&self.0.concat());
            m.complete(ChatRequest::default()).await
        }

        async fn complete_streaming(
            &self,
            request: ChatRequest,
            on_delta: &mut (dyn for<'s> FnMut(&'s str) + Send),
        ) -> Result<ChatResponse, LlmError> {
            for p in &self.0 {
                on_delta(p);
            }
            self.complete(request).await
        }
    }

    #[tokio::test]
    async fn a_value_split_across_streamed_deltas_never_reaches_the_listener() {
        let value = "sk-proj-SplitAcrossDeltas0123456789AbCd";
        assert!(redactor().register("SPLIT_TEST_KEY", value) > 0);
        let model = RedactingModel(Arc::new(Streamer(vec![
            "La chiave è sk-proj-Split",
            "AcrossDeltas01234",
            "56789AbCd e basta. Fine",
        ])));
        let mut seen = Vec::new();
        let r = model
            .complete_streaming(ChatRequest::default(), &mut |d| seen.push(d.to_owned()))
            .await
            .unwrap();
        let said = seen.concat();
        assert!(!said.contains(value), "{said}");
        assert!(!said.contains("SplitAcross"), "{said}");
        assert!(said.starts_with("La chiave è "), "{said}");
        assert!(said.ends_with("Fine"), "the held tail is flushed: {said}");
        assert!(!r.message.text().contains(value));
    }
}
