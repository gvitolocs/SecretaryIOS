//! Mistral (Lucina's own Mistral API model): the fallback for questions,
//! research and analysis when the agent asked for is out of credits, signed
//! out, unreachable or busy. One model call; it never browses and never
//! writes code.

use std::sync::Arc;

use lucina_llm::{ChatModel, ChatRequest, Message};

use super::{Agent, AgentJob, AgentReply};
use crate::types::{AgentId, Kind};

pub const BACKEND: &str = "mistral-api";

const SYSTEM: &str = "You are a careful research and analysis assistant working for Giuseppe through his personal assistant Lucina. \
Answer in the language the task asks for (English or Danish; never Italian unless the task explicitly asks for it). \
Be accurate and structured. Say plainly what you are unsure of and what may be out of date. \
You have no web browsing: never claim to have searched, browsed or checked live sources. You make no code changes. \
End a long answer with a 2-3 sentence summary.";

pub struct Mistral {
    model: Arc<dyn ChatModel>,
    max_tokens: u32,
}

impl Mistral {
    pub fn new(model: Arc<dyn ChatModel>, max_tokens: u32) -> Self {
        Self { model, max_tokens }
    }
}

#[async_trait::async_trait]
impl Agent for Mistral {
    fn id(&self) -> AgentId {
        AgentId::Mistral
    }

    fn supports(&self, kind: Kind) -> bool {
        kind != Kind::Code
    }

    async fn backend(&self) -> Result<String, String> {
        Ok(BACKEND.to_owned())
    }

    async fn run(&self, job: &AgentJob) -> Result<AgentReply, String> {
        let req = ChatRequest {
            messages: vec![Message::system(SYSTEM), Message::user(job.prompt.clone())],
            temperature: Some(0.3),
            max_tokens: Some(self.max_tokens),
            purpose: "delegate-mistral",
            ..ChatRequest::default()
        };
        let r = tokio::time::timeout(job.timeout, self.model.complete(req))
            .await
            .map_err(|_| format!("{BACKEND} timed out after {} s", job.timeout.as_secs()))?
            .map_err(|e| format!("{BACKEND}: {e}"))?;
        let text = r.message.text().trim().to_owned();
        if text.is_empty() {
            return Err(format!("{BACKEND}: empty answer"));
        }
        Ok(AgentReply {
            text,
            session: None,
            turns: 1,
            backend: BACKEND.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use lucina_llm::ScriptedModel;

    use super::*;
    use crate::config::Isolation;

    fn job(prompt: &str) -> AgentJob {
        AgentJob {
            job_id: "j".into(),
            kind: Kind::Research,
            prompt: prompt.into(),
            workdir: PathBuf::from("/tmp"),
            writable: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(5),
            max_turns: 1,
            resume_session: None,
            isolation: Isolation::None,
        }
    }

    #[tokio::test]
    async fn one_call_never_code() {
        let model = Arc::new(ScriptedModel::new());
        model.push_text("  Rust 1.95 is out.  ").push_text(" ");
        let m = Mistral::new(model.clone(), 4000);
        assert!(m.supports(Kind::Question) && m.supports(Kind::Research));
        assert!(m.supports(Kind::Analysis) && !m.supports(Kind::Code));
        assert_eq!(m.backend().await.unwrap(), "mistral-api");
        let r = m.run(&job("what is new in Rust?")).await.unwrap();
        assert_eq!(r.text, "Rust 1.95 is out.");
        assert_eq!(r.backend, "mistral-api");
        {
            let reqs = model.requests.lock().unwrap();
            assert_eq!(reqs[0].purpose, "delegate-mistral");
            assert_eq!(reqs[0].max_tokens, Some(4000));
            assert!(reqs[0].messages[0]
                .text()
                .contains("never claim to have searched"));
            assert_eq!(reqs[0].messages[1].text(), "what is new in Rust?");
        }
        assert_eq!(
            m.run(&job("again")).await.unwrap_err(),
            "mistral-api: empty answer"
        );
    }
}
