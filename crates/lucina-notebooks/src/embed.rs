//! Embeddings: EmbeddingGemma 2 (768 dimensions) behind an OpenAI-compatible
//! `/v1/embeddings` (llama.cpp `llama-server --embeddings`), local on nezopt.
//! EmbeddingGemma wants its task prompts: `title: … | text: …` for documents and
//! `task: search result | query: …` for queries. Vectors are stored at unit length, so a
//! dot product is the cosine.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Document,
}

#[async_trait]
pub trait Embedder: Send + Sync {
    /// One vector per input, in order. `titles` (documents only) may be empty.
    async fn embed(&self, kind: Kind, texts: &[String], title: &str) -> anyhow::Result<Vec<Vec<f32>>>;
    fn name(&self) -> String;
}

pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The text the model sees for one input.
pub fn prompt(kind: Kind, text: &str, title: &str) -> String {
    let text: String = text.chars().take(2400).collect();
    match kind {
        Kind::Query => format!("task: search result | query: {text}"),
        Kind::Document => {
            let title = if title.trim().is_empty() { "none" } else { title.trim() };
            format!("title: {title} | text: {text}")
        }
    }
}

pub struct HttpEmbedder {
    pub url: String,
    pub model: String,
    http: reqwest::Client,
}

impl HttpEmbedder {
    pub fn new(url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            model: model.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }
}

#[async_trait]
impl Embedder for HttpEmbedder {
    async fn embed(&self, kind: Kind, texts: &[String], title: &str) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(16) {
            let input: Vec<String> = batch.iter().map(|t| prompt(kind, t, title)).collect();
            let r = self
                .http
                .post(&self.url)
                .json(&json!({ "model": self.model, "input": input }))
                .send()
                .await?;
            let status = r.status();
            let v: Value = r.json().await?;
            if !status.is_success() {
                anyhow::bail!("embedder answered {status}: {}", v.to_string().chars().take(200).collect::<String>());
            }
            let mut data: Vec<(usize, Vec<f32>)> = v["data"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("embedder answered without data"))?
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let idx = d["index"].as_u64().map(|x| x as usize).unwrap_or(i);
                    let mut e: Vec<f32> = d["embedding"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
                        .unwrap_or_default();
                    normalize(&mut e);
                    (idx, e)
                })
                .collect();
            data.sort_by_key(|(i, _)| *i);
            if data.len() != batch.len() || data.iter().any(|(_, e)| e.is_empty()) {
                anyhow::bail!("embedder returned {} vectors for {} inputs", data.len(), batch.len());
            }
            out.extend(data.into_iter().map(|(_, e)| e));
        }
        Ok(out)
    }

    fn name(&self) -> String {
        self.model.clone()
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;

    /// Bag-of-letters vectors: enough for tests to tell texts apart.
    pub struct Letters;

    #[async_trait]
    impl Embedder for Letters {
        async fn embed(&self, _kind: Kind, texts: &[String], _title: &str) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0f32; 26];
                    for c in t.to_lowercase().bytes().filter(u8::is_ascii_lowercase) {
                        v[(c - b'a') as usize] += 1.0;
                    }
                    normalize(&mut v);
                    v
                })
                .collect())
        }
        fn name(&self) -> String {
            "letters".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_follow_embeddinggemma() {
        assert_eq!(prompt(Kind::Query, "dhcp", ""), "task: search result | query: dhcp");
        assert_eq!(prompt(Kind::Document, "x", ""), "title: none | text: x");
        assert_eq!(prompt(Kind::Document, "x", "RM 2"), "title: RM 2 | text: x");
    }

    #[test]
    fn unit_vectors() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((dot(&v, &v) - 1.0).abs() < 1e-6);
    }
}
