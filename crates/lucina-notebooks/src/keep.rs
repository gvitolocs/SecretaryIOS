//! The owner's notes stay in the one Keep store on p1 (secretary-server `/v1/notes`,
//! `crates/secretary-server/src/notes/`). A notebook links notes by slug and indexes a copy
//! of their text for search; "save to note" creates a Keep note there, labelled `notebook`.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct KeepNote {
    pub slug: String,
    pub title: String,
    pub content: String,
    pub updated_at: Option<String>,
}

#[async_trait]
pub trait Keep: Send + Sync {
    async fn get(&self, slug: &str) -> anyhow::Result<Option<KeepNote>>;
    async fn create(&self, title: &str, content: &str, labels: &[String]) -> anyhow::Result<KeepNote>;
}

pub fn excerpt(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = flat.chars().take(200).collect();
    if flat.chars().count() > 200 {
        s.push('…');
    }
    s
}

/// secretary-server with Lucina's service token (`LUCINA_SECRETARY_SERVER_URL`,
/// `LUCINA_SECRETARY_SERVICE_TOKEN_FILE`, as the notes tool uses them).
pub struct HttpKeep {
    pub base: String,
    pub token_file: PathBuf,
    http: reqwest::Client,
}

impl HttpKeep {
    pub fn new(base: impl Into<String>, token_file: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            token_file: token_file.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
        }
    }

    fn token(&self) -> anyhow::Result<String> {
        Ok(std::fs::read_to_string(&self.token_file)?.trim().to_owned())
    }
}

fn note(v: &Value) -> KeepNote {
    KeepNote {
        slug: v["slug"].as_str().unwrap_or_default().to_owned(),
        title: v["title"].as_str().unwrap_or_default().to_owned(),
        content: v["content"].as_str().unwrap_or_default().to_owned(),
        updated_at: v["updated_at"].as_str().map(str::to_owned),
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[async_trait]
impl Keep for HttpKeep {
    async fn get(&self, slug: &str) -> anyhow::Result<Option<KeepNote>> {
        let r = self
            .http
            .get(format!("{}/v1/notes/{}", self.base, enc(slug)))
            .bearer_auth(self.token()?)
            .send()
            .await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = r.status();
        let v: Value = r.json().await?;
        if !status.is_success() {
            anyhow::bail!("notes answered {status}");
        }
        Ok(Some(note(&v)))
    }

    async fn create(&self, title: &str, content: &str, labels: &[String]) -> anyhow::Result<KeepNote> {
        let r = self
            .http
            .post(format!("{}/v1/notes", self.base))
            .bearer_auth(self.token()?)
            .json(&json!({ "title": title, "content": content, "labels": labels, "source": "lucina" }))
            .send()
            .await?;
        let status = r.status();
        let v: Value = r.json().await?;
        if !status.is_success() {
            anyhow::bail!("notes answered {status}: {}", v["error"]["message"].as_str().unwrap_or(""));
        }
        Ok(note(&v))
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct Memory {
        pub notes: Mutex<HashMap<String, KeepNote>>,
    }

    impl Memory {
        pub fn with(notes: &[(&str, &str, &str)]) -> Self {
            let m = Self::default();
            for (slug, title, content) in notes {
                m.notes.lock().unwrap().insert(
                    slug.to_string(),
                    KeepNote { slug: slug.to_string(), title: title.to_string(), content: content.to_string(), updated_at: Some("2026-10-29T10:00:00Z".into()) },
                );
            }
            m
        }
    }

    #[async_trait]
    impl Keep for Memory {
        async fn get(&self, slug: &str) -> anyhow::Result<Option<KeepNote>> {
            Ok(self.notes.lock().unwrap().get(slug).cloned())
        }
        async fn create(&self, title: &str, content: &str, _labels: &[String]) -> anyhow::Result<KeepNote> {
            let slug = crate::ids::slug(title, 40);
            let n = KeepNote { slug: slug.clone(), title: title.into(), content: content.into(), updated_at: Some(crate::ids::now()) };
            self.notes.lock().unwrap().insert(slug, n.clone());
            Ok(n)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs_are_path_safe() {
        assert_eq!(super::enc("rm-class 2/x"), "rm-class%202%2Fx");
        assert_eq!(super::excerpt("a\n\nb"), "a b");
    }
}
