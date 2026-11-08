//! The language models notebooks use: DeepSeek with Lucina's own key (default) or the local
//! Qwen 3.8 27B behind the llama.cpp gateway. Both speak the OpenAI chat API, so both are a
//! `lucina_llm::DeepSeek` client with its own base URL. Never a free or anonymous route:
//! the owner's documents go only to these two.

use async_trait::async_trait;
use lucina_llm::{ChatRequest, DeepSeek, Message as LlmMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    #[default]
    Deepseek,
    Qwen,
}

impl Model {
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s.map(str::trim).unwrap_or("") {
            "" | "deepseek" | "default" => Some(Self::Deepseek),
            "qwen" | "local" => Some(Self::Qwen),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

impl Turn {
    pub fn system(t: impl Into<String>) -> Self {
        Self { role: Role::System, text: t.into() }
    }
    pub fn user(t: impl Into<String>) -> Self {
        Self { role: Role::User, text: t.into() }
    }
    pub fn assistant(t: impl Into<String>) -> Self {
        Self { role: Role::Assistant, text: t.into() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Ask {
    pub model: Model,
    pub turns: Vec<Turn>,
    pub json: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub purpose: &'static str,
}

/// Gets the answer text as it streams.
pub type Delta = Box<dyn FnMut(&str) + Send + 'static>;

#[async_trait]
pub trait Llm: Send + Sync {
    /// The answer text and the model's name. `on_delta` (when given) gets the text as it streams.
    async fn complete(
        &self,
        ask: Ask,
        on_delta: Option<Delta>,
    ) -> anyhow::Result<(String, String)>;
}

/// DeepSeek (Lucina's key) and, when configured, the local Qwen.
pub struct Clients {
    pub deepseek: DeepSeek,
    pub qwen: Option<DeepSeek>,
}

/// Reasoning models may answer `<think>…</think>` first; that never reaches a user.
pub fn strip_think(s: &str) -> String {
    let mut out = s.to_owned();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + "</think>".len(), "");
    }
    out.trim().to_owned()
}

#[async_trait]
impl Llm for Clients {
    async fn complete(
        &self,
        ask: Ask,
        on_delta: Option<Delta>,
    ) -> anyhow::Result<(String, String)> {
        let client = match ask.model {
            Model::Deepseek => &self.deepseek,
            Model::Qwen => self
                .qwen
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("the local model is not configured"))?,
        };
        let req = ChatRequest {
            messages: ask
                .turns
                .into_iter()
                .map(|t| match t.role {
                    Role::System => LlmMessage::system(t.text),
                    Role::User => LlmMessage::user(t.text),
                    Role::Assistant => LlmMessage::assistant(t.text),
                })
                .collect(),
            max_tokens: ask.max_tokens,
            temperature: ask.temperature,
            json_object: ask.json,
            purpose: ask.purpose,
            // Answers from sources don't need DeepSeek's thinking tokens.
            thinking: (ask.model == Model::Deepseek).then_some(false),
            ..Default::default()
        };
        let resp = match on_delta {
            Some(mut f) => client.stream(req, move |d| f(d)).await?,
            None => client.chat(req).await?,
        };
        Ok((strip_think(resp.message.text()), resp.model))
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    /// Answers with the queued replies in order; records what it was asked.
    #[derive(Default)]
    pub struct Scripted {
        pub replies: Mutex<Vec<String>>,
        pub asked: Mutex<Vec<Ask>>,
    }

    impl Scripted {
        pub fn new(replies: &[&str]) -> Self {
            Self {
                replies: Mutex::new(replies.iter().rev().map(|s| s.to_string()).collect()),
                asked: Mutex::default(),
            }
        }
    }

    #[async_trait]
    impl Llm for Scripted {
        async fn complete(
            &self,
            ask: Ask,
            on_delta: Option<Delta>,
        ) -> anyhow::Result<(String, String)> {
            let model = format!("{:?}", ask.model).to_lowercase();
            self.asked.lock().unwrap().push(ask);
            let r = self.replies.lock().unwrap().pop().unwrap_or_default();
            if let Some(mut f) = on_delta {
                for w in r.split_inclusive(' ') {
                    f(w);
                }
            }
            Ok((r, model))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_blocks_are_dropped() {
        assert_eq!(strip_think("<think>hmm</think>\nAnswer"), "Answer");
        assert_eq!(strip_think("No think"), "No think");
        assert_eq!(Model::parse(Some("qwen")), Some(Model::Qwen));
        assert_eq!(Model::parse(None), Some(Model::Deepseek));
        assert_eq!(Model::parse(Some("gpt")), None);
    }
}
